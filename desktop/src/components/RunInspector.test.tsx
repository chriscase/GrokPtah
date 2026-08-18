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
  diff: "diff --git a/src/lib.rs b/src/lib.rs\n+changed",
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

  it("renders live remote events and routes scoped controls", async () => {
    const onSteer = vi.fn(async () => undefined);
    const onCancel = vi.fn(async () => undefined);
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const remoteRun = run({
      runId: "remote-run",
      sessionId: "remote-session",
      workspace: "/tmp/remote",
      state: "running",
      clientId: "mcp",
      progress: {
        round: 1,
        maxRounds: 4,
        lastTool: "inspect",
        detail: "Watching the remote workspace",
        updatedAt: "2026-08-17T12:01:00Z",
      },
    });
    render(
      <RunInspector
        runs={[remoteRun]}
        remote
        liveEvents={{
          "remote-run": [
            {
              seq: 9,
              ts: "2026-08-17T12:01:09Z",
              update: {
                type: "agent_message_chunk",
                session_id: "remote-session",
                text: "Remote progress",
              },
            },
          ],
        }}
        onRefresh={vi.fn()}
        {...actions}
        onSteer={onSteer}
        onCancel={onCancel}
      />,
    );

    expect(await screen.findByText("Assistant · Remote progress")).toBeTruthy();
    fireEvent.change(screen.getByLabelText("Steering prompt"), {
      target: { value: "Keep checking the workspace" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Steer now" }));
    await waitFor(() => expect(onSteer).toHaveBeenCalledWith("remote-run", "Keep checking the workspace"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel run" }));
    await waitFor(() => expect(onCancel).toHaveBeenCalledWith("remote-run"));
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

  it("keeps completed and cancelled remote history after the live pointer changes", () => {
    render(
      <RunInspector
        runs={[
          run({ runId: "history-complete", state: "completed", clientId: "mcp" }),
          run({ runId: "history-cancel", state: "cancelled", clientId: "mcp" }),
          run({
            runId: "live-remote",
            state: "running",
            clientId: "mcp",
            progress: {
              round: 1,
              maxRounds: 4,
              lastTool: "inspect",
              detail: "New live run",
              updatedAt: "2026-08-17T12:04:00Z",
            },
          }),
        ]}
        remote
        onRefresh={vi.fn()}
        {...actions}
      />,
    );

    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("Cancelled")).toBeTruthy();
    expect(screen.getByText("Running")).toBeTruthy();
    expect(screen.getAllByText("Remote service", { selector: ".run-origin" })).toHaveLength(3);
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

    expect(screen.getByRole("alert")).toHaveTextContent("Task history could not be refreshed");
    expect(screen.getByText(/bridge unavailable/)).toBeTruthy();
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
    expect(screen.getByText("Promote reviewed changes")).toBeTruthy();
    expect(screen.getByText(/diff --git/)).toBeTruthy();
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
