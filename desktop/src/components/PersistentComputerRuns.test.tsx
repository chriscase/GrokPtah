import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ComputerCockpitSnapshot, ComputerLocalApproval } from "../lib/protocol";
import { PersistentComputerRuns, type AppOwnedComputerRun } from "./PersistentComputerRuns";

const mocks = vi.hoisted(() => ({
  pause: vi.fn(),
  takeover: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  api: {
    computerUseCockpitPause: mocks.pause,
    computerUseCockpitTakeOver: mocks.takeover,
    computerUseCockpitStop: mocks.stop,
  },
}));

const backend = {
  backendId: "deterministic_simulator",
  observe: true,
  semanticActions: true,
  textEntry: true,
  keyChords: false,
  pointerFallback: false,
  foregroundConflictCapacity: 1,
};

function snapshot(runId: string, state = "ready"): ComputerCockpitSnapshot {
  const local: ComputerLocalApproval = {
    runId,
    state,
    version: 3,
    actionCount: 0,
    limits: { maxActions: 8 },
    controlDisposition: state === "paused" ? "paused" : "agent_owned",
    target: { appId: `app.${runId}`, displayName: `Target ${runId}` },
    observation: null,
    grant: null,
    audit: [],
    lastError: null,
  };
  return { backend, origin: "desktop", projection: null, local, pendingApproval: null };
}

function binding(sessionId: string, runId: string): AppOwnedComputerRun {
  return {
    sessionId,
    sessionTitle: `Lane ${sessionId}`,
    snapshot: snapshot(runId),
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.pause.mockImplementation(async (_sessionId: string, runId: string) =>
    snapshot(runId, "paused"),
  );
  mocks.takeover.mockImplementation(async (_sessionId: string, runId: string) => {
    const next = snapshot(runId, "paused");
    next.local!.controlDisposition = "operator_takeover";
    return next;
  });
  mocks.stop.mockImplementation(async (_sessionId: string, runId: string) =>
    snapshot(runId, "cancelled"),
  );
});

afterEach(cleanup);

describe("PersistentComputerRuns", () => {
  it("keeps exact Stop and Take over controls outside the cockpit", async () => {
    const onSnapshot = vi.fn();
    const onOpen = vi.fn();
    render(
      <PersistentComputerRuns
        runs={[binding("session-a", "run-a")]}
        preferredSessionId="session-b"
        onSnapshot={onSnapshot}
        onOpen={onOpen}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => {
      expect(mocks.stop).toHaveBeenCalledWith("session-a", "run-a");
      expect(onSnapshot).toHaveBeenCalledWith(
        "session-a",
        expect.objectContaining({ local: expect.objectContaining({ runId: "run-a" }) }),
      );
    });
    fireEvent.click(screen.getByRole("button", { name: /Target run-a/ }));
    expect(onOpen).toHaveBeenCalledWith("session-a");
  });

  it("routes global emergency keys to the preferred exact session", async () => {
    render(
      <PersistentComputerRuns
        runs={[binding("session-a", "run-a"), binding("session-b", "run-b")]}
        preferredSessionId="session-b"
        onSnapshot={vi.fn()}
        onOpen={vi.fn()}
      />,
    );

    fireEvent.keyDown(window, { key: "S", ctrlKey: true, shiftKey: true });
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledWith("session-b", "run-b"));
    expect(mocks.stop).not.toHaveBeenCalledWith("session-a", "run-a");
  });

  it("does not put Stop behind another in-flight control", async () => {
    mocks.pause.mockReturnValue(new Promise(() => {}));
    render(
      <PersistentComputerRuns
        runs={[binding("session-a", "run-a")]}
        preferredSessionId="session-a"
        onSnapshot={vi.fn()}
        onOpen={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledWith("session-a", "run-a"));
  });

  it("does not render terminal Runs as active controls", () => {
    const ended = binding("session-a", "run-a");
    ended.snapshot = snapshot("run-a", "cancelled");
    render(
      <PersistentComputerRuns
        runs={[ended]}
        preferredSessionId="session-a"
        onSnapshot={vi.fn()}
        onOpen={vi.fn()}
      />,
    );
    expect(screen.queryByRole("complementary", { name: "Active Computer Runs" })).toBeNull();
  });
});
