import type { ComputerRunProjection } from "./protocol";

/**
 * Visible Computer Run activity states (#286).
 *
 * The durable control disposition already distinguishes who owns a run; this
 * derives the operator-facing presentation from the authoritative projection
 * so the cockpit never re-implements state logic from raw run fields. Every
 * disposition maps to exactly one visible state, so a run can never render as
 * "agent owned" while it is in fact stopped, interrupted, or uncertain.
 */
export type ComputerActivityTone = "active" | "idle" | "held" | "ended" | "attention";

export interface ComputerActivityState {
  /** Stable identifier for tests and styling. */
  id:
    | "agent_active"
    | "agent_idle"
    | "paused"
    | "operator_takeover"
    | "stopped"
    | "interrupted"
    | "uncertain_outcome";
  /** Short label rendered in the status chip. */
  label: string;
  /** Sentence explaining who holds control and what happens next. */
  detail: string;
  tone: ComputerActivityTone;
  /**
   * True when the agent may still act. Callers use this to decide whether a
   * Stop/Take over affordance must remain reachable.
   */
  controllable: boolean;
  /** True when the run cannot be revived and a new run is required. */
  absorbing: boolean;
}

const AGENT_ACTIVE: ComputerActivityState = {
  id: "agent_active",
  label: "Agent is using the computer",
  detail:
    "The agent is observing or acting on the authorized target. You can pause, stop, or take over at any time.",
  tone: "active",
  controllable: true,
  absorbing: false,
};

const AGENT_IDLE: ComputerActivityState = {
  id: "agent_idle",
  label: "Agent owns this run",
  detail:
    "The agent holds this run but is not currently observing or acting. You can pause, stop, or take over at any time.",
  tone: "idle",
  controllable: true,
  absorbing: false,
};

const PAUSED: ComputerActivityState = {
  id: "paused",
  label: "Paused",
  detail:
    "Authority was revoked and no action can run. Reauthorize the target to let the agent continue.",
  tone: "held",
  controllable: true,
  absorbing: false,
};

const OPERATOR_TAKEOVER: ComputerActivityState = {
  id: "operator_takeover",
  // Deliberately a summary: the dedicated takeover panel owns the full
  // explanation, and repeating it here would show the same sentence twice.
  label: "You have taken over",
  detail: "You hold control of this run. The agent cannot act on it again.",
  tone: "held",
  controllable: false,
  absorbing: true,
};

const STOPPED: ComputerActivityState = {
  id: "stopped",
  label: "Stopped",
  detail: "The run was stopped and its authority revoked. Start a new Computer Run to continue.",
  tone: "ended",
  controllable: false,
  absorbing: true,
};

const INTERRUPTED: ComputerActivityState = {
  id: "interrupted",
  label: "Interrupted",
  detail:
    "GrokPtah restarted while this run was live. Nothing resumes automatically; start a new Computer Run after checking the target.",
  tone: "attention",
  controllable: false,
  absorbing: true,
};

const UNCERTAIN: ComputerActivityState = {
  id: "uncertain_outcome",
  label: "Outcome uncertain",
  detail:
    "An action completed after the run was stopped or superseded, so its effect on the target is unknown. Check the target before starting a new run.",
  tone: "attention",
  controllable: false,
  absorbing: true,
};

/**
 * Map the authoritative projection to exactly one visible activity state.
 *
 * Control disposition wins over lifecycle state: a stopped or taken-over run
 * must never present as merely "paused" because both share the paused
 * lifecycle state.
 */
export function computerActivityState(
  projection: ComputerRunProjection,
): ComputerActivityState {
  switch (projection.controlDisposition) {
    case "operator_takeover":
      return OPERATOR_TAKEOVER;
    case "stopped":
      return STOPPED;
    case "interrupted":
      return INTERRUPTED;
    case "uncertain_outcome":
      return UNCERTAIN;
    case "paused":
      return PAUSED;
    case "agent_owned":
      break;
    default:
      // An unrecognized disposition from a newer host must not silently render
      // as an ordinary agent-owned run.
      return {
        ...UNCERTAIN,
        label: "Unrecognized control state",
        detail:
          "This run reports a control state this version does not understand. Treat it as unsafe and start a new Computer Run.",
      };
  }

  // Agent-owned runs that already reached a terminal lifecycle state are
  // finished work, not live agent control.
  if (projection.terminal) {
    return {
      ...STOPPED,
      id: "stopped",
      label: `Ended — ${projection.state.replaceAll("_", " ")}`,
      detail: "This run reached a terminal state. Start a new Computer Run to continue.",
    };
  }
  return projection.agentActive ? AGENT_ACTIVE : AGENT_IDLE;
}

/**
 * Accessible announcement for the run's activity, including the target so a
 * screen-reader user knows which application is being driven.
 */
export function computerActivityAnnouncement(
  projection: ComputerRunProjection,
): string {
  const state = computerActivityState(projection);
  return `${state.label}. Target ${projection.target.displayName}. ${state.detail}`;
}
