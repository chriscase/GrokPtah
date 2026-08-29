import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ComputerCockpit } from "./ComputerCockpit";
import type {
  ComputerCockpitSnapshot,
  ComputerRun,
  ComputerRunProjection,
} from "../lib/protocol";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  eligibility: vi.fn(),
  qualifyAgent: vi.fn(),
  proposeAgent: vi.fn(),
  cancelAgent: vi.fn(),
  status: vi.fn(),
  requestPermission: vi.fn(),
  targets: vi.fn(),
  start: vi.fn(),
  startNative: vi.fn(),
  refresh: vi.fn(),
  stage: vi.fn(),
  approve: vi.fn(),
  discard: vi.fn(),
  pause: vi.fn(),
  takeOver: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("../lib/api", () => ({
  api: {
    computerUseCockpitSnapshot: mocks.snapshot,
    computerUseCockpitAgentEligibility: mocks.eligibility,
    computerUseCockpitQualifyAgent: mocks.qualifyAgent,
    computerUseCockpitProposeAgentAction: mocks.proposeAgent,
    computerUseCockpitCancelAgent: mocks.cancelAgent,
    computerUseStatus: mocks.status,
    computerUseRequestPermission: mocks.requestPermission,
    computerUseListTargets: mocks.targets,
    computerUseCockpitStartSimulator: mocks.start,
    computerUseCockpitStartNative: mocks.startNative,
    computerUseCockpitRefresh: mocks.refresh,
    computerUseCockpitStageAction: mocks.stage,
    computerUseCockpitApprove: mocks.approve,
    computerUseCockpitDiscardApproval: mocks.discard,
    computerUseCockpitPause: mocks.pause,
    computerUseCockpitTakeOver: mocks.takeOver,
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
};

function run(state = "ready"): ComputerRun {
  return {
    runId: "run-1",
    ownerSessionId: "session-1",
    target: {
      appId: "com.grokptah.computer-use-simulator",
      windowId: "demo-form",
      generation: 1,
      displayName: "Computer Use Simulator",
      sensitivity: "none",
    },
    state,
    controlDisposition: state === "paused" ? "paused" : "agent_owned",
    controlEpoch: state === "paused" ? 1 : 0,
    version: state === "paused" ? 5 : 3,
    createdAt: "2026-08-13T10:00:00Z",
    updatedAt: "2026-08-13T10:00:01Z",
    limits: { maxActions: 8, maxDurationSecs: 600 },
    actionCount: state === "paused" ? 1 : 0,
    currentObservation:
      state === "ready"
        ? {
            observationId: "observation-1",
            sequence: 1,
            capturedAt: "2026-08-13T10:00:01Z",
            target: {
              appId: "com.grokptah.computer-use-simulator",
              windowId: "demo-form",
              generation: 1,
              displayName: "Computer Use Simulator",
              sensitivity: "none",
            },
            elements: [
              {
                elementId: "observation-1-name",
                role: "text_field",
                label: "Name",
                enabled: true,
                focused: false,
                sensitivity: "none",
                actions: ["set_value"],
              },
              {
                elementId: "observation-1-submit",
                role: "button",
                label: "Submit",
                enabled: false,
                focused: false,
                sensitivity: "none",
                actions: ["invoke"],
              },
              {
                elementId: "observation-1-status",
                role: "status",
                label: "Not submitted",
                enabled: true,
                focused: false,
                sensitivity: "none",
                actions: [],
              },
            ],
            elementsTruncated: false,
          }
        : null,
    grant: {
      grantId: "grant-1",
      expiresAt: "2026-08-13T10:02:00Z",
      usesRemaining: state === "paused" ? 0 : 1,
      revokedAt: state === "paused" ? "2026-08-13T10:00:02Z" : null,
      actionClasses: ["semantic", "text_entry"],
    },
    audit: [
      {
        sequence: 1,
        at: "2026-08-13T10:00:00Z",
        operation: "create_run",
        disposition: "accepted",
      },
    ],
  };
}

const TERMINAL_STATES = ["completed", "failed", "cancelled", "interrupted", "limit_reached"];

/**
 * Mirror of the Rust `project_run_at` derivation so fixtures exercise the same
 * authoritative shape the host actually sends, rather than a snapshot missing
 * the projection the cockpit renders its status from.
 */
function projectionFor(runValue: ComputerRun): ComputerRunProjection {
  return {
    runId: runValue.runId,
    ownerSessionId: runValue.ownerSessionId,
    parentRunId: null,
    campaignId: null,
    target: runValue.target,
    state: runValue.state,
    controlDisposition: runValue.controlDisposition ?? "agent_owned",
    controlEpoch: runValue.controlEpoch ?? 0,
    version: runValue.version,
    agentActive: ["observing", "acting"].includes(runValue.state),
    terminal: TERMINAL_STATES.includes(runValue.state),
    createdAt: runValue.createdAt,
    updatedAt: runValue.updatedAt,
    startedAt: runValue.startedAt ?? null,
    endedAt: runValue.endedAt ?? null,
    progress: {
      actionCount: runValue.actionCount,
      maxActions: runValue.limits.maxActions,
      evidenceBytes: 0,
      maxEvidenceBytes: 8 * 1024 * 1024,
      elapsedMillis: 1000,
      maxDurationSecs: runValue.limits.maxDurationSecs,
      durationExceeded: false,
    },
    grant: runValue.grant
      ? {
          grantId: runValue.grant.grantId,
          actionClasses: runValue.grant.actionClasses,
          issuedBy: "local_user",
          issuedAt: runValue.createdAt,
          expiresAt: runValue.grant.expiresAt,
          usesRemaining: runValue.grant.usesRemaining ?? null,
          revoked: Boolean(runValue.grant.revokedAt),
          expired: false,
        }
      : null,
    observation: runValue.currentObservation
      ? {
          observationId: runValue.currentObservation.observationId,
          sequence: runValue.currentObservation.sequence,
          capturedAt: runValue.currentObservation.capturedAt,
          elementCount: runValue.currentObservation.elements.length,
          elementsTruncated: runValue.currentObservation.elementsTruncated,
          sensitivity: "none",
          hasScreenshot: false,
          screenshotRedacted: null,
          stale: false,
        }
      : null,
    lastOutcome: runValue.lastOutcome
      ? { expectedPostconditionMet: runValue.lastOutcome.expectedPostconditionMet ?? null }
      : null,
    lastError: runValue.lastError ? { code: runValue.lastError.code } : null,
    eventRange: runValue.audit.length
      ? {
          startSeq: runValue.audit[0].sequence,
          endSeq: runValue.audit[runValue.audit.length - 1].sequence,
        }
      : null,
  };
}

function snapshot(runValue: ComputerRun | null = null): ComputerCockpitSnapshot {
  return {
    backend,
    origin: "desktop",
    projection: runValue ? projectionFor(runValue) : null,
    run: runValue,
    pendingApproval: null,
  };
}

const props = {
  sessionId: "session-1",
  sessionTitle: "Demo build",
  model: "grok-4.5",
  effort: "high",
  sessionBusy: false,
  onClose: vi.fn(),
  onSteer: vi.fn().mockResolvedValue("Priority prompt preserved."),
  onRunState: vi.fn(),
};

beforeEach(() => {
  vi.clearAllMocks();
  props.onSteer.mockResolvedValue("Priority prompt preserved.");
  mocks.snapshot.mockResolvedValue(snapshot());
  mocks.eligibility.mockRejectedValue(new Error("No persisted eligibility"));
  mocks.cancelAgent.mockResolvedValue(false);
  mocks.status.mockResolvedValue({
    platformId: "macos",
    available: true,
    minimumOsVersion: "13.0",
    screenRecording: "granted",
    accessibility: "granted",
    detail: null,
    executor: {
      kind: "in_process_host",
      bundleId: "com.chriscase.grokptah",
      helperBundleId: "com.chriscase.grokptah.computer-use-helper",
      version: "0.1.0",
      helperVersion: "0.1.0",
      signingClass: "uninspected",
      tccPrincipal: "com.chriscase.grokptah",
    },
  });
  mocks.requestPermission.mockResolvedValue("granted");
  mocks.targets.mockResolvedValue([]);
});

afterEach(cleanup);

describe("ComputerCockpit", () => {
  it("keeps Computer Run ownership visible beyond the focused tab", async () => {
    render(
      <ComputerCockpit
        {...props}
        scope={{
          laneId: "session-1",
          laneTitle: "Demo build",
          agentLabel: "Ad hoc",
          runtimeTarget: "local_desktop",
          runtimeConnection: "connected",
          workspacePath: "/work/grokptah",
          runLabel: "No active Run",
        }}
      />,
    );

    const scope = await screen.findByRole("group", { name: "Lane scope" });
    expect(scope).toHaveTextContent("Lane Demo build");
    expect(scope).toHaveTextContent("Agent Ad hoc");
    expect(scope).toHaveTextContent("Runtime Local desktop · Connected");
    expect(scope).toHaveTextContent("Workspace work / grokptah");
    expect(scope).toHaveTextContent("Run No active Run");
  });

  it("requires exact scope review before a run starts", async () => {
    mocks.start.mockResolvedValue(snapshot(run()));
    render(<ComputerCockpit {...props} />);

    const start = await screen.findByRole("button", { name: "Start Computer Run" });
    expect(start).toBeDisabled();
    expect(screen.getByText("com.grokptah.computer-use-simulator")).toBeTruthy();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact target and one-action scope",
      }),
    );
    expect(start).toBeEnabled();
    fireEvent.click(start);

    await waitFor(() =>
      expect(mocks.start).toHaveBeenCalledWith(
        "session-1",
        "com.grokptah.computer-use-simulator",
      ),
    );
  });

  it("binds a native run to the exact locally selected window", async () => {
    mocks.targets.mockResolvedValue([
      {
        selectionToken: "selection-1",
        target: {
          appId: "com.example.fixture",
          windowId: "macos-window-42",
          generation: 7,
          displayName: "Disposable Fixture",
          sensitivity: "none",
        },
        geometry: { x: 0, y: 0, width: 720, height: 520, scaleFactor: 1 },
        onScreen: true,
        active: true,
        minimized: false,
      },
    ]);
    mocks.startNative.mockResolvedValue(snapshot(run()));
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "macOS app" }));
    const findTargets = screen.getByRole("button", { name: "Find eligible windows" });
    await waitFor(() => expect(findTargets).toBeEnabled());
    fireEvent.click(findTargets);
    fireEvent.click(await screen.findByRole("radio", { name: /Disposable Fixture/ }));
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact target and one-action scope",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Start Computer Run" }));

    await waitFor(() =>
      expect(mocks.startNative).toHaveBeenCalledWith(
        "session-1",
        "selection-1",
        "com.example.fixture",
      ),
    );
  });

  it("blocks native discovery until required permissions are granted", async () => {
    const executor = {
      kind: "in_process_host" as const,
      bundleId: "com.chriscase.grokptah",
      helperBundleId: "com.chriscase.grokptah.computer-use-helper",
      version: "0.1.0",
      helperVersion: "0.1.0",
      signingClass: "uninspected" as const,
      tccPrincipal: "com.chriscase.grokptah",
    };
    mocks.status
      .mockResolvedValueOnce({
        platformId: "macos",
        available: true,
        minimumOsVersion: "13.0",
        screenRecording: "missing",
        accessibility: "granted",
        detail: "Screen Recording is required before window discovery.",
        executor,
      })
      .mockResolvedValueOnce({
        platformId: "macos",
        available: true,
        minimumOsVersion: "13.0",
        screenRecording: "granted",
        accessibility: "granted",
        detail: null,
        executor,
      });
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "macOS app" }));
    const findTargets = screen.getByRole("button", { name: "Find eligible windows" });
    expect(findTargets).toBeDisabled();
    expect(
      await screen.findByText(
        /Codex Computer Use and Terminal grants do not grant GrokPtah access/,
      ),
    ).toBeTruthy();
    expect(
      await screen.findByTestId("computer-executor-identity"),
    ).toHaveTextContent("com.chriscase.grokptah");
    expect(screen.getByTestId("computer-executor-identity")).toHaveTextContent(
      "not packaged helper qualification",
    );
    fireEvent.click(
      await screen.findByRole("button", {
        name: "Request Screen Recording permission",
      }),
    );

    await waitFor(() => expect(findTargets).toBeEnabled());
    expect(mocks.requestPermission).toHaveBeenCalledWith("screen_recording");
    expect(mocks.targets).not.toHaveBeenCalled();
  });

  it("shows an exact one-use approval and requires reauthorization after action", async () => {
    const ready = snapshot(run());
    const pending = {
      ...ready,
      pendingApproval: {
        approvalId: "approval-1",
        ownerSessionId: "session-1",
        runId: "run-1",
        runVersion: 3,
        observationId: "observation-1",
        targetLabel: "Computer Use Simulator",
        action: {
          type: "set_value" as const,
          element_id: "observation-1-name",
          text: "Ada Lovelace",
        },
        actionSummary: "Enter visible text in Name",
        risk: "Text entry",
        createdAt: "2026-08-13T10:00:01Z",
      },
    };
    mocks.snapshot.mockResolvedValue(ready);
    mocks.stage.mockResolvedValue(pending);
    mocks.approve.mockResolvedValue(snapshot(run("paused")));
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "Stage text entry" }));
    expect(await screen.findByText("Ada Lovelace")).toBeTruthy();
    expect(screen.getByText("Frame 1 · one use")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Approve once" }));

    expect(
      await screen.findByRole("button", { name: "Reauthorize and observe" }),
    ).toBeTruthy();
    expect(mocks.approve).toHaveBeenCalledTimes(1);
  });

  it("shows measured model eligibility without expanding local approval", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(run()));
    render(
      <ComputerCockpit
        {...props}
        computerUseTier="semantic_act"
        computerCapabilitySource="measured"
      />,
    );

    expect(await screen.findByText("Semantic Act · Measured")).toBeTruthy();
    expect(screen.getByText("One action authorized")).toBeTruthy();
  });

  it("does not offer reauthorization after operator takeover", async () => {
    mocks.snapshot.mockResolvedValue(
      snapshot({
        ...run("paused"),
        controlDisposition: "operator_takeover",
        controlEpoch: 2,
      }),
    );
    render(<ComputerCockpit {...props} />);

    expect(await screen.findByText("Take over active")).toBeTruthy();
    expect(screen.getByText(/cannot be reauthorized after takeover/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reauthorize and observe" })).toBeNull();
  });

  it("qualifies an unknown model before offering agent proposals", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(run()));
    mocks.qualifyAgent.mockResolvedValue({
      model: "grok-4.5",
      tier: "semantic_act",
      source: "session_measured",
    });
    render(<ComputerCockpit {...props} />);

    const qualify = await screen.findByRole("button", {
      name: "Verify model for this session",
    });
    fireEvent.click(qualify);

    expect(await screen.findByText("Semantic Act · Session Measured")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Propose next action" })).toBeEnabled();
  });

  it("stages one model proposal for the existing local approval", async () => {
    const active = snapshot(run());
    mocks.snapshot.mockResolvedValue(active);
    mocks.proposeAgent.mockResolvedValue({
      snapshot: {
        ...active,
        pendingApproval: {
          approvalId: "approval-model",
          ownerSessionId: "session-1",
          runId: "run-1",
          runVersion: 3,
          observationId: "observation-1",
          targetLabel: "Computer Use Simulator",
          action: {
            type: "set_value",
            element_id: "observation-1-name",
            text: "Ada Lovelace",
          },
          actionSummary: "Enter visible text in Name",
          risk: "Text entry",
          createdAt: "2026-08-13T10:00:02Z",
        },
      },
      summary: "Enter the requested visible name",
      completed: false,
    });
    render(<ComputerCockpit {...props} computerUseTier="semantic_act" computerCapabilitySource="measured" />);

    fireEvent.click(await screen.findByRole("button", { name: "Propose next action" }));

    await waitFor(() =>
      expect(mocks.proposeAgent).toHaveBeenCalledWith(
        "session-1",
        "run-1",
        3,
        "observation-1",
        "Enter Ada Lovelace in the Name field, then submit the form.",
      ),
    );
    expect(await screen.findByRole("dialog", { name: "Approve Computer Use action" })).toBeTruthy();
    expect(screen.getByText("Enter the requested visible name", { exact: false })).toBeTruthy();
  });

  it("keeps Stop available while model inference is pending", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(run()));
    mocks.proposeAgent.mockReturnValue(new Promise(() => {}));
    mocks.stop.mockResolvedValue(snapshot(run("cancelled")));
    render(<ComputerCockpit {...props} computerUseTier="semantic_act" computerCapabilitySource="measured" />);

    fireEvent.click(await screen.findByRole("button", { name: "Propose next action" }));
    expect(await screen.findByRole("button", { name: "Waiting for model" })).toBeDisabled();
    const stop = screen.getByRole("button", { name: "Stop" });
    expect(stop).toBeEnabled();
    fireEvent.click(stop);
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledWith("session-1", "run-1"));
  });

  it("discards a stale response after the owning session changes", async () => {
    let resolveOld: (value: ComputerCockpitSnapshot) => void = () => {};
    mocks.snapshot
      .mockImplementationOnce(
        () =>
          new Promise<ComputerCockpitSnapshot>((resolve) => {
            resolveOld = resolve;
          }),
      )
      .mockResolvedValueOnce(snapshot());
    const view = render(<ComputerCockpit {...props} />);
    view.rerender(
      <ComputerCockpit {...props} sessionId="session-2" sessionTitle="Other build" />,
    );
    await screen.findByText("Scope review");
    resolveOld(snapshot(run()));

    await waitFor(() => {
      expect(screen.queryByText("Frame 1")).toBeNull();
      expect(screen.getByText("Owned by Other build")).toBeTruthy();
    });
  });

  it("does not carry a model objective into another session", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(run()));
    const view = render(
      <ComputerCockpit
        {...props}
        computerUseTier="semantic_act"
        computerCapabilitySource="measured"
      />,
    );
    const objective = await screen.findByRole("textbox", { name: "Objective" });
    fireEvent.change(objective, { target: { value: "Session-private objective" } });
    expect(objective).toHaveValue("Session-private objective");

    view.rerender(
      <ComputerCockpit
        {...props}
        sessionId="session-2"
        sessionTitle="Other build"
        computerUseTier="semantic_act"
        computerCapabilitySource="measured"
      />,
    );
    await waitFor(() =>
      expect(screen.getByRole("textbox", { name: "Objective" })).toHaveValue(
        "Enter Ada Lovelace in the Name field, then submit the form.",
      ),
    );
  });

  it("keeps steering non-cancelling and session-bound", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(run()));
    render(<ComputerCockpit {...props} sessionBusy />);
    fireEvent.change(await screen.findByPlaceholderText("Guide the agent at its next safe step"), {
      target: { value: "Verify the postcondition before continuing" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Steer now" }));

    await waitFor(() =>
      expect(props.onSteer).toHaveBeenCalledWith(
        "Verify the postcondition before continuing",
      ),
    );
  });

  it("ignores steer completion after the owning session changes", async () => {
    let resolveSteer: (message: string) => void = () => {};
    props.onSteer.mockImplementationOnce(
      () =>
        new Promise<string>((resolve) => {
          resolveSteer = resolve;
        }),
    );
    mocks.snapshot.mockResolvedValueOnce(snapshot(run())).mockResolvedValueOnce(snapshot());
    const view = render(<ComputerCockpit {...props} sessionBusy />);

    fireEvent.change(await screen.findByPlaceholderText("Guide the agent at its next safe step"), {
      target: { value: "Continue after checking the result" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Steer now" }));
    view.rerender(
      <ComputerCockpit {...props} sessionId="session-2" sessionTitle="Other build" />,
    );
    await screen.findByText("Scope review");
    resolveSteer("Stale steer feedback");

    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact target and one-action scope",
      }),
    );
    await waitFor(() => {
      expect(screen.queryByText("Stale steer feedback")).toBeNull();
      expect(screen.getByRole("button", { name: "Start Computer Run" })).toBeEnabled();
    });
  });
});
