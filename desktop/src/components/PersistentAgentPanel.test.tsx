import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  PersistentAgent,
  PersistentAgentResumePlan,
} from "../lib/protocol";
import { PersistentAgentPanel } from "./PersistentAgentPanel";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const agent: PersistentAgent = {
  agentId: "agent-session-1",
  sessionId: "session-1",
  workspace: "/tmp/grokptah-demo",
  model: "grok-4.6",
  state: "waiting",
  currentRunId: null,
  latestCheckpointId: "checkpoint-1",
  continuationOrdinal: 1,
  createdAt: "2026-08-17T12:00:00Z",
  updatedAt: "2026-08-17T12:01:00Z",
};

const plan: PersistentAgentResumePlan = {
  agent,
  parentRunId: "desktop-run-1",
  checkpoint: {
    checkpointId: "checkpoint-1",
    agentId: agent.agentId,
    sessionId: agent.sessionId,
    runId: "desktop-run-1",
    parentCheckpointId: null,
    ordinal: 1,
    workspace: agent.workspace,
    contextSummary: "The previous turn completed the continuity probe.",
    contextHash: "a".repeat(64),
    eventSeq: 12,
    reason: "turn_completed",
    createdAt: agent.updatedAt,
  },
};

describe("PersistentAgentPanel", () => {
  it("inspects a checkpoint and requires an explicit resume prompt", async () => {
    const onInspect = vi.fn(async () => plan);
    const onResume = vi.fn(async () => "Resumed successfully");
    vi.spyOn(window, "confirm").mockReturnValue(true);

    render(
      <PersistentAgentPanel
        agents={[agent]}
        activeSessionId={agent.sessionId}
        onRefresh={vi.fn()}
        onOpenSession={vi.fn()}
        onInspect={onInspect}
        onResume={onResume}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Inspect checkpoint" }));
    expect(await screen.findByText(plan.checkpoint.contextSummary)).toBeTruthy();

    const resumeButton = screen.getByRole("button", { name: "Resume agent" });
    expect(resumeButton).toBeDisabled();
    fireEvent.change(screen.getByLabelText("Resume prompt for agent-session-1"), {
      target: { value: "Continue from the verified checkpoint" },
    });
    expect(resumeButton).not.toBeDisabled();
    fireEvent.click(resumeButton);

    await waitFor(() =>
      expect(onResume).toHaveBeenCalledWith(
        agent,
        "Continue from the verified checkpoint",
      ),
    );
    expect(onInspect).toHaveBeenCalledWith(agent.sessionId);
  });

  it("does not enable resume for an active agent", async () => {
    const active = { ...agent, state: "active" as const };
    render(
      <PersistentAgentPanel
        agents={[active]}
        activeSessionId={active.sessionId}
        onRefresh={vi.fn()}
        onOpenSession={vi.fn()}
        onInspect={vi.fn(async () => ({ ...plan, agent: active }))}
        onResume={vi.fn(async () => "never")}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Inspect checkpoint" }));
    const resumeButton = await screen.findByRole("button", { name: "Resume agent" });
    fireEvent.change(screen.getByLabelText("Resume prompt for agent-session-1"), {
      target: { value: "Do not run" },
    });
    expect(resumeButton).toBeDisabled();
  });
});
