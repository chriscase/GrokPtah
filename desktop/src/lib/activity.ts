/** Turn activity phases driven by real session://update events. */

export type ActivityPhase =
  | "idle"
  | "queued" // prompt sent, no server event yet
  | "thinking" // thought chunks arriving
  | "streaming" // assistant text chunks
  | "tool" // tool call running
  | "permission" // waiting on user approval
  | "done" // turn finished cleanly (brief celebration)
  | "error";

export interface ActivityState {
  phase: ActivityPhase;
  /** Short human label */
  label: string;
  /** Optional detail (tool name, thought snippet) */
  detail?: string;
  /** Epoch ms of last activity signal (event or send) */
  lastEventAt: number;
  /** True while a turn is in flight (not idle/done after settle) */
  live: boolean;
}

export function idleActivity(at = Date.now()): ActivityState {
  return {
    phase: "idle",
    label: "Ready",
    lastEventAt: at,
    live: false,
  };
}

export function queuedActivity(at = Date.now()): ActivityState {
  return {
    phase: "queued",
    label: "Waiting on server",
    detail: "Prompt sent — awaiting first response…",
    lastEventAt: at,
    live: true,
  };
}

export function doneActivity(cancelled: boolean, at = Date.now()): ActivityState {
  return {
    phase: "done",
    label: cancelled ? "Stopped" : "Done",
    detail: cancelled
      ? "Turn cancelled"
      : "Server finished — ready for your next message",
    lastEventAt: at,
    live: false,
  };
}

export function errorActivity(message: string, at = Date.now()): ActivityState {
  return {
    phase: "error",
    label: "Error",
    detail: message.slice(0, 160),
    lastEventAt: at,
    live: false,
  };
}

/** If still live but no events for a while, surface “still waiting”. */
export function stalledDetail(activity: ActivityState, now = Date.now()): string | null {
  if (!activity.live) return null;
  const silent = now - activity.lastEventAt;
  if (silent < 2500) return null;
  if (silent < 8000) {
    return "Still waiting for the next server event…";
  }
  if (silent < 20000) {
    return "No events for a few seconds — connection may be slow…";
  }
  return "No server activity for a while — check network or stop the turn";
}

export function phaseFromThought(): Pick<ActivityState, "phase" | "label"> {
  return { phase: "thinking", label: "Thinking" };
}

export function phaseFromStream(): Pick<ActivityState, "phase" | "label"> {
  return { phase: "streaming", label: "Receiving" };
}

export function phaseFromTool(title: string): Pick<ActivityState, "phase" | "label" | "detail"> {
  return {
    phase: "tool",
    label: "Working",
    detail: title,
  };
}

export function phaseFromPermission(): Pick<ActivityState, "phase" | "label" | "detail"> {
  return {
    phase: "permission",
    label: "Needs approval",
    detail: "Waiting for your decision…",
  };
}
