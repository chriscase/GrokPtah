import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DurableRun, DurableRunEventPage, RunReview } from "../lib/protocol";
import { RunInspector } from "./RunInspector";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

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
  diff: [
    "diff --git a/src/lib.rs b/src/lib.rs",
    "--- a/src/lib.rs",
    "+++ b/src/lib.rs",
    "@@ -4,2 +4,3 @@",
    " fn existing() {}",
    "+fn added() {}",
  ].join("\n"),
  diffTruncated: false,
  fingerprint: "abc123",
};

const eventPage: DurableRunEventPage = {
  entries: [
    {
      seq: 4,
      ts: "2026-08-11T12:00:04Z",
      update: {
        type: "agent_progress",
        session_id: "session-1",
        round: 2,
        max_rounds: 8,
        last_tool: "write_files",
        detail: "Updated src/lib.rs",
      },
    },
  ],
  nextCursor: 4,
  cursorExpired: false,
};

const actions = {
  onReview: vi.fn(async () => review),
  onApprove: vi.fn(async () => undefined),
  onPromote: vi.fn(async () => undefined),
  onDiscard: vi.fn(async () => undefined),
  onRetry: vi.fn(async () => undefined),
  onSteer: vi.fn(async () => undefined),
  onCancel: vi.fn(async () => undefined),
  onEvents: vi.fn(async () => eventPage),
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

  it("shows the durable admission position for queued coordinator work", () => {
    render(
      <RunInspector
        runs={[run({ state: "queued", clientId: "mcp", queuePosition: 2 })]}
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    expect(screen.getByText("Queued")).toBeTruthy();
    expect(screen.getByRole("status")).toHaveTextContent("Waiting for an execution slot · position 2");
  });

  it("labels explicit post-restart retries", () => {
    render(
      <RunInspector
        runs={[run({ retryOf: "interrupted-source" })]}
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    expect(screen.getByRole("status")).toHaveTextContent(
      "Explicit retry of interrupted run interrupted-source",
    );
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

  it("shows a current-session refresh failure with a retry action", () => {
    const onRefresh = vi.fn();
    render(
      <RunInspector
        runs={[]}
        error="Could not refresh durable runs: bridge unavailable"
        onRefresh={onRefresh}
        {...actions}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("bridge unavailable");
    fireEvent.click(screen.getByRole("button", { name: "Retry refresh" }));
    expect(onRefresh).toHaveBeenCalledTimes(1);
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

  it("requires a fresh prompt before retrying an MCP run", async () => {
    const onRefresh = vi.fn();
    const onRetry = vi.fn(async () => undefined);
    render(
      <RunInspector
        runs={[run({ state: "interrupted", clientId: "mcp", runId: "mcp-interrupted" })]}
        onRefresh={onRefresh}
        {...actions}
        onRetry={onRetry}
      />,
    );

    const button = screen.getByRole("button", { name: "Retry interrupted run" });
    expect(button).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Fresh recovery prompt"), {
      target: { value: "Continue by checking the test failure" },
    });
    expect(button).not.toBeDisabled();
    fireEvent.click(button);
    expect(onRetry).toHaveBeenCalledWith(
      "mcp-interrupted",
      "Continue by checking the test failure",
    );
    await waitFor(() => expect(onRefresh).toHaveBeenCalledTimes(1));
  });

  it("steers without cancelling and confirms MCP cancellation", async () => {
    const onRefresh = vi.fn();
    const onSteer = vi.fn(async () => undefined);
    const onCancel = vi.fn(async () => undefined);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(
      <RunInspector
        runs={[run({ state: "running", clientId: "mcp", runId: "mcp-running" })]}
        onRefresh={onRefresh}
        {...actions}
        onSteer={onSteer}
        onCancel={onCancel}
      />,
    );

    const steerButton = screen.getByRole("button", { name: "Steer now" });
    expect(steerButton).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Steering prompt"), {
      target: { value: "Keep the current plan and verify the tests" },
    });
    expect(steerButton).not.toBeDisabled();
    fireEvent.click(steerButton);
    expect(onSteer).toHaveBeenCalledWith(
      "mcp-running",
      "Keep the current plan and verify the tests",
    );
    await waitFor(() => expect(onRefresh).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "Cancel run" }));
    expect(onCancel).toHaveBeenCalledWith("mcp-running");
    await waitFor(() => expect(onRefresh).toHaveBeenCalledTimes(2));
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
    // The review is now per file, so a reviewer steps through the change
    // rather than scanning one wall of diff text before promoting.
    expect(screen.getByTestId("run-diff-position")).toHaveTextContent(
      "File 1 of 1 · src/lib.rs · modified · +1 −0",
    );
    expect(screen.getByTestId("run-diff-hunks")).toHaveTextContent("fn added() {}");
    expect(screen.queryByTestId("run-diff-open")).toBeNull();
  });

  it("blocks promotion until complete evidence is acknowledged", async () => {
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

    fireEvent.click(screen.getByText("Review diff"));
    const promote = await screen.findByTestId(`run-promote-${isolated.runId}`);
    expect(promote).toBeDisabled();
    expect(promote).toHaveAttribute(
      "title",
      "Blocked until complete diff evidence is reviewed and acknowledged",
    );
  });

  it("enables promotion once complete evidence is acknowledged", async () => {
    const onPromote = vi.fn();
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
    render(
      <RunInspector runs={[isolated]} onRefresh={vi.fn()} {...actions} onPromote={onPromote} />,
    );

    fireEvent.click(screen.getByText("Review diff"));
    fireEvent.click(await screen.findByTestId("run-diff-acknowledge"));

    const promote = screen.getByTestId(`run-promote-${isolated.runId}`);
    expect(promote).not.toBeDisabled();
    expect(promote).toHaveAttribute(
      "title",
      "Apply the reviewed changes to the source workspace",
    );
    fireEvent.click(promote);
    await waitFor(() => expect(onPromote).toHaveBeenCalledWith(isolated.runId));
  });

  it("keeps promotion blocked when the diff evidence is incomplete", async () => {
    const onReview = vi.fn(async () => ({ ...review, diffTruncated: true }));
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
    render(
      <RunInspector runs={[isolated]} onRefresh={vi.fn()} {...actions} onReview={onReview} />,
    );

    fireEvent.click(screen.getByText("Review diff"));
    expect(await screen.findByTestId("run-diff-incomplete")).toBeInTheDocument();
    expect(screen.getByTestId("run-diff-acknowledge")).toBeDisabled();
    expect(screen.getByTestId(`run-promote-${isolated.runId}`)).toBeDisabled();
  });

  it("a source viewer failure does not change what may be promoted", async () => {
    // The real handler routes failures into the viewer's own state; the
    // inspector never learns about them. What matters is that opening a file —
    // successfully or not — leaves the review, the acknowledgement, and the
    // promotion gate exactly as they were.
    const onOpenSource = vi.fn();
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
    render(
      <RunInspector
        runs={[isolated]}
        onRefresh={vi.fn()}
        {...actions}
        onOpenSource={onOpenSource}
      />,
    );

    fireEvent.click(screen.getByText("Review diff"));
    fireEvent.click(await screen.findByTestId("run-diff-acknowledge"));
    expect(screen.getByTestId(`run-promote-${isolated.runId}`)).not.toBeDisabled();

    fireEvent.click(screen.getByTestId("run-diff-open"));
    expect(onOpenSource).toHaveBeenCalledOnce();

    // The review, the acknowledgement, and the promotion gate are untouched:
    // a viewer failure is not a change in authorization.
    expect(screen.getByText(/1 changed files/)).toBeTruthy();
    expect(screen.getByTestId("run-diff-acknowledge")).toBeChecked();
    expect(screen.getByTestId(`run-promote-${isolated.runId}`)).not.toBeDisabled();
  });

  it("opens a reviewed file in that run's own isolated worktree", async () => {
    const onOpenSource = vi.fn();
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
    render(
      <RunInspector
        runs={[isolated]}
        onRefresh={vi.fn()}
        onOpenSource={onOpenSource}
        {...actions}
      />,
    );

    fireEvent.click(screen.getByText("Review diff"));
    fireEvent.click(await screen.findByTestId("run-diff-open"));

    expect(onOpenSource).toHaveBeenCalledWith(isolated.runId, "src/lib.rs", 5);
    // Opening a file for reading never enables promotion by itself.
    expect(screen.getByTestId(`run-promote-${isolated.runId}`)).toBeDisabled();
  });

  it("shows durable MCP approval state before allowing promotion", async () => {
    const mcpRun = run({
      clientId: "mcp",
      execution: {
        mode: "isolated_worktree",
        sourceWorkspace: "/tmp/demo",
        executionWorkspace: "/tmp/demo/.grokptah/worktrees/runs/mcp-1",
        baseRevision: "base",
        sourceFingerprint: "source",
        finalFingerprint: "abc123",
        promotionState: "ready",
        promotedAt: null,
      },
      approval: {
        approvalId: "approval-1",
        runId: "desktop-run-1",
        sessionId: "session-1",
        workspace: "/tmp/demo",
        sourceFingerprint: "source",
        finalFingerprint: "abc123",
        changedFiles: review.changedFiles,
        issuedAt: "2026-08-11T12:00:00Z",
        expiresAt: "2099-08-11T12:05:00Z",
      },
    });
    render(
      <RunInspector
        runs={[mcpRun]}
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    expect(screen.getByText(/Approval active until/)).toBeTruthy();
    expect(screen.queryByText("Promote reviewed changes")).toBeNull();
    fireEvent.click(screen.getByText("Review diff"));
    expect(await screen.findByText("Promote reviewed changes")).toBeTruthy();
  });

  it("surfaces an isolated promotion conflict as a blocking state", () => {
    render(
      <RunInspector
        runs={[
          run({
            clientId: "mcp",
            execution: {
              mode: "isolated_worktree",
              sourceWorkspace: "/tmp/demo",
              executionWorkspace: "/tmp/demo/.grokptah/worktrees/runs/conflict",
              baseRevision: "base",
              sourceFingerprint: "source",
              finalFingerprint: "changed",
              promotionState: "conflicted",
              promotedAt: null,
            },
          }),
        ]}
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent("Promotion blocked");
    expect(screen.queryByText("Review diff")).toBeNull();
  });

  it("replays and paginates a durable run timeline", async () => {
    const onEvents = vi
      .fn()
      .mockResolvedValueOnce(eventPage)
      .mockResolvedValueOnce({ ...eventPage, entries: [], nextCursor: null });
    render(
      <RunInspector runs={[run()]} onRefresh={vi.fn()} {...actions} onEvents={onEvents} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Replay events" }));
    expect(await screen.findByText(/Round 2\/8/)).toBeTruthy();
    expect(onEvents).toHaveBeenCalledWith("desktop-run-1", 0, 80);

    fireEvent.click(screen.getByRole("button", { name: "Load more events" }));
    expect(await screen.findByText(/Run timeline · 1 events/)).toBeTruthy();
    expect(onEvents).toHaveBeenLastCalledWith("desktop-run-1", 4, 80);
  });

  it("reports honest cursor expiry without hiding durable summaries", async () => {
    const onEvents = vi.fn(async () => ({
      entries: [],
      nextCursor: null,
      cursorExpired: true,
    }));
    render(
      <RunInspector runs={[run()]} onRefresh={vi.fn()} {...actions} onEvents={onEvents} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Replay events" }));
    expect(await screen.findByText(/Older events are no longer retained/)).toBeTruthy();
    expect(screen.getByText("Handoff")).toBeTruthy();
  });
});
