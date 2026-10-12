import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DurableWorkItem, RemoteWorkSnapshot } from "../lib/protocol";
import { WorkBoard } from "./WorkBoard";

const scope = {
  laneId: "lane-1",
  laneTitle: "Product Lane",
  agentLabel: "agent-1",
  runtimeTarget: "local_desktop" as const,
  runtimeConnection: "connected" as const,
  workspacePath: "/tmp/project",
  runLabel: "Ready",
};

const item: DurableWorkItem = {
  schemaVersion: 1,
  workId: "work-1",
  kind: "implementation",
  objective: "Build the operations center",
  sessionId: "lane-1",
  workspace: "/tmp/project",
  createdBy: "coordinator",
  assignedAgentId: "agent-1",
  priority: 2,
  deadline: null,
  parentWorkId: null,
  dependencies: [],
  policy: {
    bounds: { maxPromptBytes: 10_000, maxRounds: 10, maxDurationMs: 60_000 },
    retry: { maxAttempts: 3, retryFailed: true, retryExpired: true, backoffMs: 100 },
    requiresApproval: true,
    maxConcurrentAttempts: 1,
  },
  state: "awaiting_approval",
  revision: 2,
  attemptCount: 1,
  progress: { summary: "Waiting for review", percent: 80, updatedAt: "2026-08-18T12:00:00Z" },
  result: null,
  createdAt: "2026-08-18T11:00:00Z",
  updatedAt: "2026-08-18T12:00:00Z",
};

const snapshot: RemoteWorkSnapshot = {
  work: item,
  attempts: [{
    schemaVersion: 1,
    attemptId: "attempt-1",
    workId: item.workId,
    attemptNumber: 1,
    claimantId: "agent-1",
    acquiredAt: "2026-08-18T11:01:00Z",
    leaseExpiresAt: "2026-08-18T13:00:00Z",
    lastHeartbeatAt: "2026-08-18T12:00:00Z",
    state: "awaiting_approval",
    linkedRunIds: ["run-12345678"],
    progress: item.progress,
    result: null,
    terminalReason: null,
    createdAt: "2026-08-18T11:01:00Z",
    updatedAt: "2026-08-18T12:00:00Z",
  }],
};

describe("WorkBoard", () => {
  afterEach(() => cleanup());

  it("shows durable ownership, state, and attempt history", () => {
    render(
      <WorkBoard
        items={[item]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );

    expect(screen.getByRole("heading", { name: "Build the operations center" })).toBeInTheDocument();
    expect(screen.getAllByText("awaiting approval").length).toBeGreaterThan(1);
    expect(screen.getByText("Attempt history")).toBeInTheDocument();
    expect(screen.getByText(/lease until/)).toBeInTheDocument();
  });

  it("filters to work needing attention and opens linked runs", async () => {
    const user = userEvent.setup();
    const onOpenRun = vi.fn();
    render(
      <WorkBoard
        items={[item]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onOpenRun={onOpenRun}
      />,
    );

    await user.click(screen.getByRole("button", { name: "Needs attention" }));
    expect(screen.getAllByText("Build the operations center").length).toBeGreaterThan(1);
    await user.click(screen.getByRole("button", { name: /Inspect Run/ }));
    expect(onOpenRun).toHaveBeenCalledWith("run-12345678");
  });

  it("renders a recoverable empty state", () => {
    render(
      <WorkBoard
        items={[]}
        selectedWorkId={null}
        snapshot={null}
        scope={scope}
        onRefresh={vi.fn()}
      />,
    );
    expect(screen.getByText("No durable Work Items in this Lane")).toBeInTheDocument();
  });

  it("keeps approval and assignment as explicit human actions", async () => {
    const user = userEvent.setup();
    const onAssign = vi.fn().mockResolvedValue(undefined);
    const onApprove = vi.fn().mockResolvedValue(undefined);
    render(
      <WorkBoard
        items={[item]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        mutationsEnabled
        onAssign={onAssign}
        onApprove={onApprove}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );

    const agentInput = screen.getByRole("textbox", { name: "Assigned Agent" });
    await user.clear(agentInput);
    await user.type(agentInput, "review-agent");
    await user.click(screen.getByRole("button", { name: "Update assignment" }));
    expect(onAssign).toHaveBeenCalledWith(item.workId, "review-agent", item.revision);
    await user.click(screen.getByRole("button", { name: "Approve completion" }));
    expect(onApprove).toHaveBeenCalledWith(item.workId, undefined, item.revision);
  });

  it("prepares without a selected agent and refreshes the settled review", async () => {
    const user = userEvent.setup();
    const onVerifiedChange = vi.fn(async (action: string) => {
      if (action === "prepare") {
        return {
          readiness: { ready: true, workersDispatched: 0, providerInvocations: 0 },
          safeAction: "Resolve readiness, then start one supervised attempt.",
        };
      }
      if (action === "start") {
        return {
          workId: "work-verified",
          workState: "running",
          readiness: { ready: true, workersDispatched: 0, providerInvocations: 0 },
          phases: { checksPassed: false, applied: false },
          boundedDiff: "",
        };
      }
      return {
        workId: "work-verified",
        workState: "awaiting_approval",
        candidateDigest: "sha256:candidate",
        boundedDiff: "src/ledger.rs repaired",
        phases: {
          workerStopped: true,
          changeProposed: true,
          checksPassed: true,
          humanApproved: true,
          applied: false,
        },
        checkResults: [{ checkId: "balance-regression", outcome: "passed" }],
        safeAction: "Approval is recorded. Application is a separate action.",
      };
    });
    render(
      <WorkBoard
        items={[]}
        selectedWorkId={null}
        snapshot={null}
        scope={scope}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
        onVerifiedChange={onVerifiedChange}
      />,
    );
    const prepare = screen.getByRole("button", { name: "Prepare" });
    expect(prepare).toBeDisabled();
    await user.type(screen.getByLabelText("Objective"), "Repair the balance pair");
    expect(prepare).toBeEnabled();
    await user.click(prepare);
    expect(onVerifiedChange).toHaveBeenCalledWith(
      "prepare",
      expect.objectContaining({ agentId: "", objective: "Repair the balance pair" }),
    );
    await user.click(screen.getByRole("button", { name: "Start one attempt" }));
    expect(await screen.findByText("src/ledger.rs repaired")).toBeTruthy();
    expect(screen.getByText(/balance-regression: passed/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Apply exact candidate" }));
    expect(onVerifiedChange).toHaveBeenCalledWith(
      "apply",
      expect.objectContaining({ workId: "work-verified", candidateDigest: "sha256:candidate" }),
    );
  });

  it("labels a partial page as loaded and loads another page", async () => {
    const user = userEvent.setup();
    const onLoadMore = vi.fn();
    const second = { ...item, workId: "work-2", objective: "Second loaded item", revision: 1 };
    render(
      <WorkBoard
        items={[item, second]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        traversalStatus="partial"
        onLoadMore={onLoadMore}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getByText("2 loaded")).toBeInTheDocument();
    expect(screen.queryByText(/Background refresh is holding/)).not.toBeInTheDocument();
    expect(screen.queryByText("2 total")).not.toBeInTheDocument();
    expect(screen.getByText("Counts and filters cover loaded items only.")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("2 work items loaded. The lane is not complete.");
    await user.click(screen.getByRole("button", { name: "Load more" }));
    expect(onLoadMore).toHaveBeenCalledOnce();
  });

  it("tells the reader that background refresh is holding loaded pages", () => {
    const second = { ...item, workId: "work-2", objective: "Second loaded item", revision: 1 };
    render(
      <WorkBoard
        items={[item, second]}
        selectedWorkId="work-2"
        snapshot={snapshot}
        scope={scope}
        traversalStatus="complete"
        pagesHeld
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getAllByText("Second loaded item").length).toBeGreaterThan(0);
    expect(screen.getByText("Background refresh is holding these loaded pages. Refresh replaces the list.")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Background refresh is holding loaded pages.");
    expect(screen.getByRole("button", { name: "↻" })).toBeEnabled();
  });

  it("announces a complete lane as a total", () => {
    render(
      <WorkBoard
        items={[item]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        traversalStatus="complete"
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getByText("1 total")).toBeInTheDocument();
    expect(screen.queryByText("Counts and filters cover loaded items only.")).not.toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("1 work items. This lane list is complete.");
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("asks for a refresh when the continuation expired and does not offer load more", async () => {
    const user = userEvent.setup();
    const onRetryTraversal = vi.fn();
    const onLoadMore = vi.fn();
    render(
      <WorkBoard
        items={[item]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        traversalStatus="expired"
        error="MCP remote error: restart_required"
        onLoadMore={onLoadMore}
        onRetryTraversal={onRetryTraversal}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getByText("Work list needs a fresh start")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("continuation expired");
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Refresh" }));
    expect(onRetryTraversal).toHaveBeenCalledOnce();
    expect(onLoadMore).not.toHaveBeenCalled();
  });

  it("retries a failed traversal without treating the loaded count as the lane total", async () => {
    const user = userEvent.setup();
    const onRetryTraversal = vi.fn();
    render(
      <WorkBoard
        items={[item]}
        selectedWorkId={item.workId}
        snapshot={snapshot}
        scope={scope}
        traversalStatus="error"
        error="connection reset"
        onRetryTraversal={onRetryTraversal}
        onRefresh={vi.fn()}
        onSelect={vi.fn()}
      />,
    );
    expect(screen.getByText("1 loaded")).toBeInTheDocument();
    expect(screen.getByText("Counts and filters cover loaded items only.")).toBeInTheDocument();
    expect(screen.getByText("Work could not be refreshed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Retry" }));
    expect(onRetryTraversal).toHaveBeenCalledOnce();
  });
});
