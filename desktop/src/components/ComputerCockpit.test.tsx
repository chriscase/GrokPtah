import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ComputerCockpit } from "./ComputerCockpit";
import type {
  ComputerCockpitSnapshot,
  ComputerLocalApproval,
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
  measureBackground: vi.fn(),
  startBackground: vi.fn(),
  refresh: vi.fn(),
  stage: vi.fn(),
  approve: vi.fn(),
  discard: vi.fn(),
  pause: vi.fn(),
  takeOver: vi.fn(),
  reconcile: vi.fn(),
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
    computerUseMeasureBackgroundTextEntry: mocks.measureBackground,
    computerUseCockpitStartMeasuredBackground: mocks.startBackground,
    computerUseCockpitRefresh: mocks.refresh,
    computerUseCockpitStageAction: mocks.stage,
    computerUseCockpitApprove: mocks.approve,
    computerUseCockpitDiscardApproval: mocks.discard,
    computerUseCockpitPause: mocks.pause,
    computerUseCockpitTakeOver: mocks.takeOver,
    computerUseCockpitReconcileUncertainSurface: mocks.reconcile,
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

function localView(
  state = "ready",
  extras: Partial<ComputerLocalApproval> = {},
): ComputerLocalApproval {
  return {
    runId: "run-1",
    state,
    version: state === "paused" ? 5 : 3,
    actionCount: state === "paused" ? 1 : 0,
    limits: { maxActions: 8 },
    controlDisposition: extras.controlDisposition ?? (state === "paused" ? "paused" : "agent_owned"),
    target: {
      appId: "com.grokptah.computer-use-simulator",
      displayName: "Computer Use Simulator",
    },
    observation:
      state === "ready"
        ? {
            observationId: "observation-1",
            sequence: 1,
            capturedAt: "2026-08-13T10:00:01Z",
            elements: [
              {
                elementId: "observation-1-name",
                role: "text_field",
                label: "Name",
                enabled: true,
                actions: ["set_value"],
              },
              {
                elementId: "observation-1-submit",
                role: "button",
                label: "Submit",
                enabled: false,
                actions: ["invoke"],
              },
              {
                elementId: "observation-1-status",
                role: "status",
                label: "Not submitted",
                enabled: true,
                actions: [],
              },
            ],
          }
        : null,
    grant: {
      expiresAt: "2026-08-13T10:02:00Z",
      revokedAt: state === "paused" ? "2026-08-13T10:00:02Z" : null,
      actionClasses: ["semantic", "text_entry"],
    },
    audit: [
      {
        sequence: 1,
        at: "2026-08-13T10:00:00Z",
        surfaceEvent: "run_created",
        operation: "create_run",
        disposition: "accepted",
      },
    ],
    lastError: null,
    ...extras,
  };
}

const TERMINAL_STATES = ["completed", "failed", "cancelled", "interrupted", "limit_reached"];

/**
 * Mirror of the Rust `project_run_at` derivation so fixtures exercise the same
 * authoritative shape the host actually sends, rather than a snapshot missing
 * the projection the cockpit renders its status from.
 */
function projectionFor(local: ComputerLocalApproval): ComputerRunProjection {
  return {
    runId: local.runId,
    ownerSessionId: "session-1",
    parentRunId: null,
    campaignId: null,
    target: {
      appId: local.target.appId,
      windowId: "demo-form",
      generation: 1,
      displayName: local.target.displayName,
      sensitivity: "none",
    },
    state: local.state,
    controlDisposition: local.controlDisposition ?? "agent_owned",
    controlEpoch:
      local.controlDisposition === "operator_takeover"
        ? 2
        : local.controlDisposition === "paused"
          ? 1
          : 0,
    version: local.version,
    agentActive: ["observing", "acting"].includes(local.state),
    terminal: TERMINAL_STATES.includes(local.state),
    createdAt: "2026-08-13T10:00:00Z",
    updatedAt: "2026-08-13T10:00:01Z",
    startedAt: "2026-08-13T10:00:01Z",
    endedAt: null,
    progress: {
      actionCount: local.actionCount,
      maxActions: local.limits.maxActions,
      evidenceBytes: 0,
      maxEvidenceBytes: 8 * 1024 * 1024,
      elapsedMillis: 1000,
      maxDurationSecs: 600,
      durationExceeded: false,
    },
    grant: local.grant
      ? {
          grantId: "grant-1",
          actionClasses: local.grant.actionClasses,
          issuedBy: "local_user",
          issuedAt: "2026-08-13T10:00:00Z",
          expiresAt: local.grant.expiresAt,
          usesRemaining: local.grant.revokedAt ? 0 : 1,
          revoked: Boolean(local.grant.revokedAt),
          expired: false,
        }
      : null,
    observation: local.observation
      ? {
          observationId: local.observation.observationId,
          sequence: local.observation.sequence,
          capturedAt: local.observation.capturedAt,
          elementCount: local.observation.elements.length,
          elementsTruncated: false,
          sensitivity: "none",
          hasScreenshot: false,
          screenshotRedacted: null,
          stale: false,
        }
      : null,
    lastOutcome: null,
    lastError: local.lastError ? { code: local.lastError.code } : null,
    eventRange: local.audit.length
      ? {
          startSeq: local.audit[0].sequence,
          endSeq: local.audit[local.audit.length - 1].sequence,
        }
      : null,
  };
}

function snapshot(local: ComputerLocalApproval | null = null): ComputerCockpitSnapshot {
  return {
    backend,
    origin: "desktop",
    projection: local ? projectionFor(local) : null,
    local,
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
  });
  mocks.requestPermission.mockResolvedValue("granted");
  mocks.targets.mockResolvedValue([]);
});

afterEach(cleanup);

describe("ComputerCockpit", () => {
  it("sanitizes backend error details and offers a retry", async () => {
    mocks.snapshot
      .mockRejectedValueOnce(
        new Error(
          "Computer Use storage is unavailable: /Users/chriscase/.grokptah/computer-use is already open (Resource temporarily unavailable (os error 35))",
        ),
      )
      .mockResolvedValueOnce(snapshot(localView()));

    render(<ComputerCockpit {...props} />);

    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Computer Run storage is busy.");
    expect(alert).not.toHaveTextContent("/Users/chriscase");

    fireEvent.click(screen.getByRole("button", { name: "Retry Computer Run" }));
    await screen.findByText("Frame 1");
    expect(mocks.snapshot).toHaveBeenCalledTimes(2);
  });

  it("reopens only the exact app-owned Run binding", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
    const onSnapshot = vi.fn();
    render(
      <ComputerCockpit
        {...props}
        boundRunId="run-1"
        onSnapshot={onSnapshot}
      />,
    );

    await screen.findByText("Frame 1");
    expect(mocks.snapshot).toHaveBeenCalledWith("session-1", "run-1");
    expect(onSnapshot).toHaveBeenCalledWith(
      "session-1",
      expect.objectContaining({ local: expect.objectContaining({ runId: "run-1" }) }),
    );
  });

  it("renders typed replay events and keeps emergency controls usable across a history gap", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
    mocks.stop.mockResolvedValue(snapshot(localView("cancelled")));
    render(
      <ComputerCockpit
        {...props}
        eventReplay={{
          runId: "run-1",
          cursor: 9,
          gapDetected: true,
          replayedEntries: 2,
          lastEvent: "permission_revoked",
        }}
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Event history is incomplete",
    );
    expect(screen.getByText("Run Created")).toBeTruthy();
    expect(screen.getByText(/Permission Revoked · through #9/)).toBeTruthy();
    const stop = screen.getByRole("button", { name: "Stop" });
    expect(stop).toBeEnabled();
    fireEvent.click(stop);
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledWith("session-1", "run-1"));
  });

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

  it("explains durable inter-agent surface queueing without exposing lease handles", async () => {
    const next = snapshot(localView());
    next.coordination = {
      state: "queued",
      queuePosition: 2,
      queueDepth: 3,
      ownsSurface: false,
      blockedByUncertainOutcome: false,
      active: {
        agentId: "agent-reviewer",
        workId: "work-review",
        runId: "run-review",
      },
      expiresAt: "2026-08-13T10:01:00Z",
      updatedAt: "2026-08-13T10:00:05Z",
    };
    mocks.snapshot.mockResolvedValue(next);

    render(<ComputerCockpit {...props} />);

    const status = await screen.findByRole("region", {
      name: "Waiting for the shared surface",
    });
    expect(status).toHaveTextContent("Agent agent-reviewer is using it");
    expect(status).toHaveTextContent("2 of 3");
    expect(status).toHaveTextContent("agent-reviewer");
    expect(status).not.toHaveTextContent("lease-");
    expect(status).not.toHaveTextContent("attempt-");
  });

  it("requires exact scope review before a run starts", async () => {
    mocks.start.mockResolvedValue(snapshot(localView()));
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
    mocks.startNative.mockResolvedValue(snapshot(localView()));
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

  it("calibrates and binds one exact measured-background text-entry run", async () => {
    mocks.targets.mockResolvedValue([
      {
        selectionToken: "selection-background",
        target: {
          appId: "com.example.disposable",
          windowId: "macos-window-background",
          generation: 9,
          displayName: "Disposable Background Fixture",
          sensitivity: "none",
        },
        geometry: { x: 0, y: 0, width: 720, height: 520, scaleFactor: 1 },
        onScreen: true,
        active: false,
        minimized: false,
      },
    ]);
    mocks.measureBackground.mockResolvedValue({
      measurementToken: "measurement-1",
      target: {
        appId: "com.example.disposable",
        windowId: "macos-window-background",
        generation: 9,
        displayName: "Disposable Background Fixture",
        sensitivity: "none",
      },
      supportedActionClasses: ["text_entry"],
      validForMillis: 120_000,
    });
    mocks.startBackground.mockResolvedValue(snapshot(localView()));
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "macOS app" }));
    const findTargets = screen.getByRole("button", { name: "Find eligible windows" });
    await waitFor(() => expect(findTargets).toBeEnabled());
    fireEvent.click(findTargets);
    fireEvent.click(
      await screen.findByRole("radio", { name: /Disposable Background Fixture/ }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "Measured background text" }),
    );
    expect(screen.getByRole("button", { name: "Start Computer Run" })).toBeDisabled();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "This exact target is disposable and may be changed and restored",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Calibrate and restore" }));
    await waitFor(() =>
      expect(mocks.measureBackground).toHaveBeenCalledWith(
        "session-1",
        "selection-background",
        "com.example.disposable",
        "Project label",
        "grokptah-background-probe",
        true,
      ),
    );
    expect(await screen.findByText("Measured for this exact target")).toBeTruthy();
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "I reviewed this exact target and one-action scope",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Start Computer Run" }));
    await waitFor(() =>
      expect(mocks.startBackground).toHaveBeenCalledWith(
        "session-1",
        "selection-background",
        "measurement-1",
        "com.example.disposable",
      ),
    );
  });

  it("does not calibrate a background target while it is active", async () => {
    mocks.targets.mockResolvedValue([
      {
        selectionToken: "selection-active",
        target: {
          appId: "com.example.active",
          windowId: "macos-window-active",
          generation: 10,
          displayName: "Active Fixture",
          sensitivity: "none",
        },
        geometry: { x: 0, y: 0, width: 720, height: 520, scaleFactor: 1 },
        onScreen: true,
        active: true,
        minimized: false,
      },
    ]);
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "macOS app" }));
    const findTargets = screen.getByRole("button", { name: "Find eligible windows" });
    await waitFor(() => expect(findTargets).toBeEnabled());
    fireEvent.click(findTargets);
    fireEvent.click(await screen.findByRole("radio", { name: /Active Fixture/ }));
    fireEvent.click(
      screen.getByRole("button", { name: "Measured background text" }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", {
        name: "This exact target is disposable and may be changed and restored",
      }),
    );
    expect(screen.getByRole("button", { name: "Calibrate and restore" })).toBeDisabled();
    expect(screen.getByText(/Put another app in front/)).toBeTruthy();
    expect(mocks.measureBackground).not.toHaveBeenCalled();
  });

  it("blocks native discovery until required permissions are granted", async () => {
    mocks.status
      .mockResolvedValueOnce({
        platformId: "macos",
        available: true,
        minimumOsVersion: "13.0",
        screenRecording: "missing",
        accessibility: "granted",
        detail: "Screen Recording is required before window discovery.",
      })
      .mockResolvedValueOnce({
        platformId: "macos",
        available: true,
        minimumOsVersion: "13.0",
        screenRecording: "granted",
        accessibility: "granted",
        detail: null,
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
    const ready = snapshot(localView());
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
        proposalOrigin: "operator" as const,
        createdAt: "2026-08-13T10:00:01Z",
      },
    };
    mocks.snapshot.mockResolvedValue(ready);
    mocks.stage.mockResolvedValue(pending);
    mocks.approve.mockResolvedValue(snapshot(localView("paused")));
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "Stage text entry" }));
    expect(await screen.findByText("Ada Lovelace")).toBeTruthy();
    expect(screen.getByText("Frame 1 · one use")).toBeTruthy();
    expect(screen.queryByTestId("computer-agent-cursor")).toBeNull();
    const approvalDialog = screen.getByRole("dialog", { name: "Enter visible text in Name" });
    const reject = screen.getByRole("button", { name: "Reject" });
    const approve = screen.getByRole("button", { name: "Approve once" });
    expect(reject).toHaveFocus();
    approve.focus();
    fireEvent.keyDown(approvalDialog, { key: "Tab" });
    expect(reject).toHaveFocus();
    reject.focus();
    fireEvent.keyDown(approvalDialog, { key: "Tab", shiftKey: true });
    expect(approve).toHaveFocus();
    fireEvent.click(screen.getByRole("button", { name: "Approve once" }));

    expect(
      await screen.findByRole("button", { name: "Reauthorize and observe" }),
    ).toBeTruthy();
    expect(mocks.approve).toHaveBeenCalledWith(
      "session-1",
      "run-1",
      "approval-1",
      expect.any(String),
    );
  });

  it("shows measured model eligibility without expanding local approval", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
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

  it("lets keyboard users reject a pending approval with Escape", async () => {
    const ready = snapshot(localView());
    const pending = {
      ...ready,
      pendingApproval: {
        approvalId: "approval-escape",
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
        proposalOrigin: "operator" as const,
        createdAt: "2026-08-13T10:00:01Z",
      },
    };
    mocks.snapshot.mockResolvedValue(ready);
    mocks.stage.mockResolvedValue(pending);
    mocks.discard.mockResolvedValue(ready);
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "Stage text entry" }));
    const dialog = await screen.findByRole("dialog", { name: "Enter visible text in Name" });
    fireEvent.keyDown(dialog, { key: "Escape" });

    await waitFor(() =>
      expect(mocks.discard).toHaveBeenCalledWith("session-1", "run-1"),
    );
    expect(screen.queryByRole("dialog", { name: "Enter visible text in Name" })).toBeNull();
  });

  it("does not offer reauthorization after operator takeover", async () => {
    mocks.snapshot.mockResolvedValue(
      snapshot({
        ...localView("paused"),
        controlDisposition: "operator_takeover",
      }),
    );
    render(<ComputerCockpit {...props} />);

    expect(await screen.findByText("Take over active")).toBeTruthy();
    expect(screen.getByText(/cannot be reauthorized after takeover/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Reauthorize and observe" })).toBeNull();
  });

  it("requires an explicit bounded note before releasing an uncertain surface fence", async () => {
    const uncertain = snapshot({
      ...localView("failed"),
      controlDisposition: "uncertain_outcome",
      lastError: { code: "uncertain_outcome" },
    });
    uncertain.reconciliation = {
      leaseId: "lease-uncertain",
      expectedRevision: 4,
      surfaceId: "surface-1",
      incarnation: "incarnation-1",
    };
    mocks.snapshot.mockResolvedValue(uncertain);
    mocks.reconcile.mockResolvedValue(snapshot(localView("failed")));
    render(<ComputerCockpit {...props} />);

    expect(await screen.findByRole("heading", { name: "Outcome needs local confirmation" })).toBeTruthy();
    const release = screen.getByRole("button", { name: "Quarantine and release fence" });
    expect(release).toBeDisabled();
    fireEvent.change(screen.getByRole("textbox", { name: "Operator confirmation note" }), {
      target: { value: "I verified this exact surface is clear." },
    });
    expect(release).toBeEnabled();
    fireEvent.click(release);

    await waitFor(() =>
      expect(mocks.reconcile).toHaveBeenCalledWith(
        "session-1",
        "run-1",
        "lease-uncertain",
        4,
        "surface-1",
        "incarnation-1",
        "I verified this exact surface is clear.",
      ),
    );
  });

  it("qualifies an unknown model before offering agent proposals", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
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
    const active = snapshot(localView());
    mocks.snapshot.mockResolvedValue(active);
    mocks.proposeAgent.mockResolvedValue({
      snapshot: {
        ...active,
        local: {
          ...active.local!,
          audit: [
            ...active.local!.audit,
            {
              sequence: 2,
              at: "2026-08-13T10:00:02Z",
              surfaceEvent: "action_proposed",
              operation: "action_proposed",
              disposition: "staged",
              actionClass: "text_entry",
              observationId: "observation-1",
            },
            {
              sequence: 3,
              at: "2026-08-13T10:00:02Z",
              surfaceEvent: "attention_moved",
              attention: {
                xBasisPoints: 3000,
                yBasisPoints: 1700,
                target: "semantic_element",
              },
              operation: "attention",
              disposition: "moved",
              actionClass: "text_entry",
              observationId: "observation-1",
            },
            {
              sequence: 4,
              at: "2026-08-13T10:00:02Z",
              surfaceEvent: "approval_required",
              operation: "approval",
              disposition: "required",
              actionClass: "text_entry",
              observationId: "observation-1",
            },
          ],
        },
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
          proposalOrigin: "agent",
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
    const dialog = await screen.findByRole("dialog", { name: "Enter visible text in Name" });
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-labelledby")).toBe("computer-approval-title");
    expect(dialog.getAttribute("aria-describedby")).toBe("computer-approval-details");
    expect(screen.getByText("Enter the requested visible name", { exact: false })).toBeTruthy();
    expect(screen.getByText("Agent attention · Name")).toBeTruthy();
    expect(
      screen.getByText(/Agent attention is on Name inside the authorized GrokPtah surface/),
    ).toBeTruthy();
    const marker = screen.getByTestId("computer-agent-cursor");
    expect(marker).toHaveStyle({ left: "30%", top: "17%" });

    mocks.discard.mockResolvedValue(active);
    fireEvent.click(screen.getByRole("button", { name: "Reject" }));
    await waitFor(() => expect(mocks.discard).toHaveBeenCalledWith("session-1", "run-1"));
    expect(screen.queryByTestId("computer-agent-cursor")).toBeNull();
  });

  it("marks the exact native semantic row without drawing a fake pointer", async () => {
    const local = localView();
    local.target = { appId: "com.example.editor", displayName: "Example Editor" };
    local.audit.push({
      sequence: 2,
      at: "2026-08-13T10:00:02Z",
      surfaceEvent: "attention_moved",
      attention: {
        xBasisPoints: 3000,
        yBasisPoints: 1700,
        target: "semantic_element",
      },
      operation: "attention",
      disposition: "moved",
      actionClass: "text_entry",
      observationId: "observation-1",
    });
    const active = snapshot(local);
    active.pendingApproval = {
      approvalId: "approval-native-model",
      ownerSessionId: "session-1",
      runId: "run-1",
      runVersion: 3,
      observationId: "observation-1",
      targetLabel: "Example Editor",
      action: {
        type: "set_value",
        element_id: "observation-1-name",
        text: "Ada Lovelace",
      },
      actionSummary: "Enter visible text in Name",
      risk: "Text entry",
      proposalOrigin: "agent",
      createdAt: "2026-08-13T10:00:02Z",
    };
    mocks.snapshot.mockResolvedValue(active);

    render(<ComputerCockpit {...props} />);

    expect(await screen.findByText("Agent attention · Name")).toBeTruthy();
    expect(screen.getByText("↖ Agent")).toBeTruthy();
    expect(screen.queryByTestId("computer-agent-cursor")).toBeNull();
  });

  it("does not reuse an old attention point for a newer geometry-free proposal", async () => {
    const local = localView();
    local.audit.push(
      {
        sequence: 2,
        at: "2026-08-13T10:00:02Z",
        surfaceEvent: "action_proposed",
        operation: "action_proposed",
        disposition: "staged",
        actionClass: "text_entry",
        observationId: "observation-1",
      },
      {
        sequence: 3,
        at: "2026-08-13T10:00:02Z",
        surfaceEvent: "attention_moved",
        attention: {
          xBasisPoints: 3000,
          yBasisPoints: 1700,
          target: "semantic_element",
        },
        operation: "attention",
        disposition: "moved",
        actionClass: "text_entry",
        observationId: "observation-1",
      },
      {
        sequence: 4,
        at: "2026-08-13T10:00:03Z",
        surfaceEvent: "approval_rejected",
        operation: "approval",
        disposition: "rejected",
        actionClass: "text_entry",
        observationId: "observation-1",
      },
      {
        sequence: 5,
        at: "2026-08-13T10:00:04Z",
        surfaceEvent: "action_proposed",
        operation: "action_proposed",
        disposition: "staged",
        actionClass: "text_entry",
        observationId: "observation-1",
      },
      {
        sequence: 6,
        at: "2026-08-13T10:00:04Z",
        surfaceEvent: "approval_required",
        operation: "approval",
        disposition: "required",
        actionClass: "text_entry",
        observationId: "observation-1",
      },
    );
    const active = snapshot(local);
    active.pendingApproval = {
      approvalId: "approval-without-point",
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
      proposalOrigin: "agent",
      createdAt: "2026-08-13T10:00:04Z",
    };
    mocks.snapshot.mockResolvedValue(active);

    render(<ComputerCockpit {...props} />);

    expect(await screen.findByText("Agent attention · Name")).toBeTruthy();
    expect(screen.queryByTestId("computer-agent-cursor")).toBeNull();
  });

  it("keeps Stop available while model inference is pending", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
    mocks.proposeAgent.mockReturnValue(new Promise(() => {}));
    mocks.stop.mockResolvedValue(snapshot(localView("cancelled")));
    render(<ComputerCockpit {...props} computerUseTier="semantic_act" computerCapabilitySource="measured" />);

    fireEvent.click(await screen.findByRole("button", { name: "Propose next action" }));
    expect(await screen.findByRole("button", { name: "Waiting for model" })).toBeDisabled();
    const stop = screen.getByRole("button", { name: "Stop" });
    expect(stop).toBeEnabled();
    fireEvent.click(stop);
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledWith("session-1", "run-1"));
  });

  it("keeps Stop and Take over actionable while an approved action is still in flight", async () => {
    const pending = snapshot(localView());
    pending.pendingApproval = {
      approvalId: "approval-inflight",
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
    };
    mocks.snapshot.mockResolvedValue(pending);
    let rejectApprove: (reason: Error) => void = () => {};
    mocks.approve.mockReturnValue(
      new Promise((_resolve, reject) => {
        rejectApprove = reject;
      }),
    );
    mocks.takeOver.mockResolvedValue(
      snapshot(
        localView("paused", {
          controlDisposition: "operator_takeover",
        }),
      ),
    );
    mocks.stop.mockResolvedValue(snapshot(localView("cancelled")));
    render(<ComputerCockpit {...props} />);

    fireEvent.click(await screen.findByRole("button", { name: "Approve once" }));
    await waitFor(() => expect(mocks.approve).toHaveBeenCalledTimes(1));

    const takeOver = screen.getByRole("button", { name: "Take over" });
    const stop = screen.getByRole("button", { name: "Stop" });
    expect(takeOver).toBeEnabled();
    expect(stop).toBeEnabled();
    expect(takeOver).toHaveAttribute("aria-keyshortcuts", "Control+Shift+T");
    expect(stop).toHaveAttribute("aria-keyshortcuts", "Control+Shift+S");

    fireEvent.click(takeOver);
    await waitFor(() =>
      expect(mocks.takeOver).toHaveBeenCalledWith("session-1", "run-1"),
    );
    await act(async () => {
      rejectApprove(new Error("late action result must not replace takeover"));
      await Promise.resolve();
    });
    expect(screen.queryByText("late action result must not replace takeover")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledWith("session-1", "run-1"));
  });

  it("provides out-of-band keyboard paths for Stop and Take over", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
    mocks.takeOver.mockReturnValue(new Promise(() => {}));
    mocks.stop.mockReturnValue(new Promise(() => {}));
    render(<ComputerCockpit {...props} />);
    await screen.findByText("Frame 1");

    fireEvent.keyDown(window, { key: "T", ctrlKey: true, shiftKey: true });
    fireEvent.keyDown(window, { key: "S", ctrlKey: true, shiftKey: true });

    await waitFor(() => {
      expect(mocks.takeOver).toHaveBeenCalledWith("session-1", "run-1");
      expect(mocks.stop).toHaveBeenCalledWith("session-1", "run-1");
    });
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
    resolveOld(snapshot(localView()));

    await waitFor(() => {
      expect(screen.queryByText("Frame 1")).toBeNull();
      expect(screen.getByText("Owned by Other build")).toBeTruthy();
    });
  });

  it("does not carry a model objective into another session", async () => {
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
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
    mocks.snapshot.mockResolvedValue(snapshot(localView()));
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
    mocks.snapshot.mockResolvedValueOnce(snapshot(localView())).mockResolvedValueOnce(snapshot());
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
