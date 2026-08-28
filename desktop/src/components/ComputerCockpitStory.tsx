import { useState } from "react";
import { api } from "../lib/api";
import type {
  ComputerAction,
  ComputerCockpitSnapshot,
  ComputerRun,
} from "../lib/protocol";
import { ComputerCockpit } from "./ComputerCockpit";

const target = {
  appId: "com.grokptah.computer-use-simulator",
  windowId: "demo-form",
  generation: 1,
  displayName: "Computer Use Simulator",
  sensitivity: "none",
};

const nativeTarget = {
  appId: "com.chriscase.grokptah.computer-use-demo",
  windowId: "macos-window-42",
  generation: 7,
  displayName: "GrokPtah Computer Use Demo",
  sensitivity: "none",
};

function observedRun(
  sequence: number,
  name = "",
  submitted = false,
): ComputerRun {
  const observationId = `story-observation-${sequence}`;
  return {
    runId: "story-run",
    ownerSessionId: "story-session",
    target,
    state: "ready",
    version: sequence * 2 + 1,
    createdAt: new Date(Date.now() - 22_000).toISOString(),
    updatedAt: new Date().toISOString(),
    startedAt: new Date(Date.now() - 20_000).toISOString(),
    limits: { maxActions: 8, maxDurationSecs: 600 },
    actionCount: Math.max(0, sequence - 1),
    currentObservation: {
      observationId,
      sequence,
      capturedAt: new Date().toISOString(),
      target,
      elements: [
        {
          elementId: `${observationId}-name`,
          role: "text_field",
          label: "Name",
          value: name || null,
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: ["set_value"],
        },
        {
          elementId: `${observationId}-submit`,
          role: "button",
          label: "Submit",
          enabled: Boolean(name),
          focused: false,
          sensitivity: "none",
          actions: ["invoke"],
        },
        {
          elementId: `${observationId}-status`,
          role: "status",
          label: submitted ? `Submitted for ${name}` : "Not submitted",
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: [],
        },
      ],
      elementsTruncated: false,
    },
    grant: {
      grantId: `story-grant-${sequence}`,
      expiresAt: new Date(Date.now() + 120_000).toISOString(),
      usesRemaining: 1,
      revokedAt: null,
      actionClasses: ["semantic", "text_entry"],
    },
    audit: [
      {
        sequence: 1,
        at: new Date(Date.now() - 22_000).toISOString(),
        operation: "create_run",
        disposition: "accepted",
      },
      {
        sequence: 2,
        at: new Date(Date.now() - 21_000).toISOString(),
        operation: "authorize",
        disposition: "granted",
      },
      {
        sequence: 3,
        at: new Date(Date.now() - 20_000).toISOString(),
        operation: "observe",
        disposition: "completed",
        observationId,
      },
    ],
  };
}

function nativeObservedRun(sequence: number, name = "public-demo-value", submittedValue = false): ComputerRun {
  const observationId = `native-observation-${sequence}`;
  return {
    ...observedRun(sequence, name, submittedValue),
    target: nativeTarget,
    currentObservation: {
      observationId,
      sequence,
      capturedAt: new Date().toISOString(),
      target: nativeTarget,
      elements: [
        {
          elementId: `${observationId}-element-3`,
          role: "AXTextField",
          label: "Project label",
          value: name,
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: ["set_value"],
        },
        {
          elementId: `${observationId}-element-7`,
          role: "AXPopUpButton",
          label: "Priority",
          value: "Normal",
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: ["select"],
        },
        {
          elementId: `${observationId}-element-8`,
          role: "AXButton",
          label: "Submit fixture",
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: ["invoke"],
        },
        {
          elementId: `${observationId}-element-9`,
          role: "AXStaticText",
          label: submittedValue ? `Submitted ${name} at Normal` : "Not submitted",
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: [],
        },
        {
          elementId: `${observationId}-element-12`,
          role: "AXScrollArea",
          label: "Accessible demo rows",
          enabled: true,
          focused: false,
          sensitivity: "none",
          actions: ["scroll"],
        },
      ],
      elementsTruncated: false,
    },
  };
}

const backend = {
  backendId: "deterministic_simulator",
  observe: true,
  semanticActions: true,
  textEntry: true,
  keyChords: false,
  pointerFallback: false,
};

const nativeBackend = {
  ...backend,
  backendId: "macos_accessibility_semantic",
};

let current: ComputerCockpitSnapshot = {
  backend,
  origin: "desktop",
  run: null,
  pendingApproval: null,
};
let currentName = "";
let submitted = false;
let agentQualified = false;

function setRun(run: ComputerRun | null): ComputerCockpitSnapshot {
  current = { ...current, run, pendingApproval: null };
  return structuredClone(current);
}

function pausedAfter(action: ComputerAction): ComputerRun {
  const run = structuredClone(current.run!);
  if (action.type === "set_value") currentName = action.text;
  if (action.type === "invoke") submitted = true;
  run.state = "paused";
  run.version += 1;
  run.actionCount += 1;
  run.currentObservation = null;
  run.grant = run.grant
    ? { ...run.grant, usesRemaining: 0, revokedAt: new Date().toISOString() }
    : null;
  run.lastOutcome = {
    summary:
      action.type === "set_value"
        ? "set visible demo text"
        : action.type === "activate_target"
          ? "activated the authorized application"
          : action.type === "invoke"
            ? "invoked the selected element"
            : action.type === "select"
              ? "selected the chosen value"
              : "scrolled the selected element into view",
    expectedPostconditionMet: true,
  };
  run.audit = [
    ...run.audit,
    {
      sequence: run.audit.length + 1,
      at: new Date().toISOString(),
      operation: "act",
      disposition: "completed",
      actionClass: action.type === "set_value" ? "text_entry" : "semantic",
    },
  ];
  return run;
}

api.computerUseCockpitSnapshot = async () => structuredClone(current);
api.computerUseCockpitAgentEligibility = async () => ({
  model: "grok-4.5",
  tier: agentQualified ? "semantic_act" : "none",
  source: agentQualified ? "session_measured" : "unknown",
});
api.computerUseCockpitQualifyAgent = async () => {
  agentQualified = true;
  return {
    model: "grok-4.5",
    tier: "semantic_act",
    source: "session_measured",
  };
};
api.computerUseCockpitCancelAgent = async () => false;
api.computerUseStatus = async () => ({
  platformId: "macos",
  available: true,
  minimumOsVersion: "13.0",
  screenRecording: "granted",
  accessibility: "granted",
  detail: null,
});
api.computerUseRequestPermission = async () => "granted";
api.computerUseListTargets = async () => [
  {
    selectionToken: "story-native-selection",
    target: nativeTarget,
    geometry: { x: 180, y: 120, width: 720, height: 520, scaleFactor: 2 },
    onScreen: true,
    active: true,
    minimized: false,
  },
];
api.computerUseCockpitStartSimulator = async () => {
  current = { ...current, backend };
  return setRun(observedRun(1));
};
api.computerUseCockpitStartNative = async () => {
  current = { ...current, backend: nativeBackend };
  return setRun(nativeObservedRun(1));
};
api.computerUseCockpitStageAction = async (
  _sessionId,
  runId,
  expectedVersion,
  observationId,
  action,
) => {
  const actionElementLabel =
    "element_id" in action
      ? current.run?.currentObservation?.elements.find(
          (element) => element.elementId === action.element_id,
        )?.label
      : null;
  current = {
    ...current,
    pendingApproval: {
      approvalId: crypto.randomUUID(),
      ownerSessionId: "story-session",
      runId,
      runVersion: expectedVersion,
      observationId,
      targetLabel: current.run?.target.displayName ?? target.displayName,
      action,
      actionSummary:
        action.type === "set_value"
          ? `Enter visible text in ${actionElementLabel ?? "the selected field"}`
          : action.type === "activate_target"
            ? "Bring the authorized application to the foreground"
            : `Stage ${action.type.replaceAll("_", " ")}`,
      risk:
        action.type === "set_value"
          ? "Text entry"
          : action.type === "activate_target"
            ? "Application focus"
            : "Semantic action",
      executionEnvelope: "story-opaque-execution-envelope",
      createdAt: new Date().toISOString(),
    },
  };
  return structuredClone(current);
};
api.computerUseCockpitProposeAgentAction = async (
  sessionId,
  runId,
  expectedVersion,
  observationId,
) => {
  const elementId = current.run?.currentObservation?.elements.find(
    (element) => element.actions.includes("set_value"),
  )?.elementId;
  if (!elementId) throw new Error("No visible text field is available");
  const snapshot = await api.computerUseCockpitStageAction(
    sessionId,
    runId,
    expectedVersion,
    observationId,
    { type: "set_value", element_id: elementId, text: "Ada Lovelace" },
  );
  return {
    snapshot,
    summary: "Enter the requested name in the visible field",
    completed: false,
  };
};
api.computerUseCockpitApprove = async () =>
  setRun(pausedAfter(current.pendingApproval!.action));
api.computerUseCockpitDiscardApproval = async () => {
  current = { ...current, pendingApproval: null };
  return structuredClone(current);
};
api.computerUseCockpitRefresh = async () =>
  setRun(
    current.run?.target.appId === nativeTarget.appId
      ? nativeObservedRun((current.run?.actionCount ?? 0) + 1, currentName || "public-demo-value", submitted)
      : observedRun((current.run?.actionCount ?? 0) + 1, currentName, submitted),
  );
api.computerUseCockpitPause = async () => {
  const run = structuredClone(current.run!);
  run.state = "paused";
  run.version += 1;
  run.currentObservation = null;
  if (run.grant) run.grant.revokedAt = new Date().toISOString();
  return setRun(run);
};
api.computerUseCockpitTakeOver = api.computerUseCockpitPause;
api.computerUseCockpitStop = async () => {
  const run = structuredClone(current.run!);
  run.state = "cancelled";
  run.version += 1;
  run.currentObservation = null;
  run.endedAt = new Date().toISOString();
  return setRun(run);
};

export function ComputerCockpitStory() {
  const [closed, setClosed] = useState(false);
  if (closed) {
    return (
      <main className="computer-story-closed">
        <button type="button" onClick={() => setClosed(false)}>Open Computer Run</button>
      </main>
    );
  }
  return (
    <main className="computer-story-shell">
      <ComputerCockpit
        sessionId="story-session"
        sessionTitle="Disposable demo build"
        model="grok-4.5"
        effort="high"
        sessionBusy
        onClose={() => setClosed(true)}
        onSteer={async () => "Steering will apply at the next safe step."}
      />
    </main>
  );
}
