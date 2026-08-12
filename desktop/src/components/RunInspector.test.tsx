import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DurableRun } from "../lib/protocol";
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

describe("RunInspector", () => {
  it("shows durable status, evidence, and bounded handoff", () => {
    render(<RunInspector runs={[run()]} onRefresh={vi.fn()} />);

    expect(screen.getByText("Completed")).toBeTruthy();
    expect(screen.getByText("Fix the failing test")).toBeTruthy();
    expect(screen.getByText("1 files")).toBeTruthy();
    expect(screen.getByText("1/1 tests passed")).toBeTruthy();
    expect(screen.getByText("Verification: verified")).toBeTruthy();
    expect(screen.getByText("Handoff")).toBeTruthy();
  });

  it("makes an interrupted run actionable and refreshable", () => {
    const onRefresh = vi.fn();
    render(
      <RunInspector
        runs={[run({ state: "interrupted", errorCode: "interrupted" })]}
        onRefresh={onRefresh}
      />,
    );

    expect(screen.getByText("Interrupted")).toBeTruthy();
    expect(screen.getByText(/stopped after restart/i)).toBeTruthy();
    fireEvent.click(screen.getByLabelText("Refresh task runs"));
    expect(onRefresh).toHaveBeenCalledTimes(1);
  });
});
