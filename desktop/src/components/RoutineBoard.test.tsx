import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RoutineBoard } from "./RoutineBoard";
import type { DurableRoutine } from "../lib/protocol";

const routine: DurableRoutine = {
  schemaVersion: 1,
  routineId: "routine-1",
  name: "manual-check",
  agentId: "agent-1",
  sessionId: "00000000-0000-0000-0000-000000000001",
  workspace: "/tmp/project",
  lifecycle: "enabled",
  trigger: { kind: "manual" },
  workTemplate: {
    kind: "routine",
    objective: "Create eligible Work",
    priority: 0,
    policy: {
      bounds: { maxPromptBytes: 1000, maxRounds: 4, maxDurationMs: 1000 },
      retry: { maxAttempts: 3, retryFailed: true, retryExpired: true, backoffMs: 0 },
      requiresApproval: false,
      maxConcurrentAttempts: 1,
    },
  },
  missedRunPolicy: "skip",
  concurrency: { maxInFlight: 1, overlap: "skip" },
  retry: { backoffMs: 5000, maxBackoffMs: 60000, circuitFailures: 5 },
  revision: 1,
  consecutiveFailures: 0,
  circuitOpen: false,
  createdBy: "desktop",
  createdAt: "2026-01-01T00:00:00Z",
  updatedAt: "2026-01-01T00:00:00Z",
};

describe("RoutineBoard", () => {
  it("requests fire and lifecycle changes without claiming to schedule", () => {
    const onFire = vi.fn().mockResolvedValue(undefined);
    render(
      <RoutineBoard
        items={[routine]}
        selectedRoutineId={routine.routineId}
        snapshot={{ routine, activations: [] }}
        mutationsEnabled
        onRefresh={() => undefined}
        onSelect={() => undefined}
        onFire={onFire}
        onSetLifecycle={vi.fn().mockResolvedValue(undefined)}
      />,
    );
    expect(
      screen.getByText(/runtime-home owner fires schedules/i),
    ).toBeInTheDocument();
    screen.getByRole("button", { name: "Fire now" }).click();
    expect(onFire).toHaveBeenCalledWith("routine-1");
  });
});
