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

const backend = {
  backendId: "deterministic_simulator",
  observe: true,
  semanticActions: true,
  textEntry: true,
  keyChords: false,
  pointerFallback: false,
};

let current: ComputerCockpitSnapshot = {
  backend,
  origin: "desktop",
  run: null,
  pendingApproval: null,
};
let currentName = "";
let submitted = false;

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
    summary: action.type === "set_value" ? "set demo name" : "submitted demo form",
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
api.computerUseCockpitStartSimulator = async () => setRun(observedRun(1));
api.computerUseCockpitStageAction = async (
  _sessionId,
  runId,
  expectedVersion,
  observationId,
  action,
) => {
  current = {
    ...current,
    pendingApproval: {
      approvalId: crypto.randomUUID(),
      ownerSessionId: "story-session",
      runId,
      runVersion: expectedVersion,
      observationId,
      targetLabel: target.displayName,
      action,
      actionSummary:
        action.type === "set_value" ? "Enter visible text in Name" : "Invoke Submit",
      risk: action.type === "set_value" ? "Text entry" : "Semantic action",
      createdAt: new Date().toISOString(),
    },
  };
  return structuredClone(current);
};
api.computerUseCockpitApprove = async () =>
  setRun(pausedAfter(current.pendingApproval!.action));
api.computerUseCockpitDiscardApproval = async () => {
  current = { ...current, pendingApproval: null };
  return structuredClone(current);
};
api.computerUseCockpitRefresh = async () =>
  setRun(observedRun((current.run?.actionCount ?? 0) + 1, currentName, submitted));
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
