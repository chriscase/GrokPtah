import type {
  CodingWorktreeDisposition,
  CodingWorktreeHostView,
  CodingWorktreePhase,
} from "./protocol";

/**
 * Operator-facing enablement for Coding Worktree disposition chrome.
 *
 * Mirrors host invariants: no session → no actions; Uncertain/Paused/Settled
 * cannot auto-retry or auto-Accept; Stop stays legal until Destroyed.
 * This module does not invent disposition semantics.
 */
export type CodingWorktreeStatus =
  | "none"
  | CodingWorktreePhase
  | "uncertain";

export type CodingWorktreeAffordances = {
  status: CodingWorktreeStatus;
  canPause: boolean;
  canStop: boolean;
  canAccept: boolean;
  canDiscard: boolean;
  canKeepForReview: boolean;
  reason: string;
};

function canSettle(view: CodingWorktreeHostView): boolean {
  return (
    view.phase === "active" &&
    !view.pauseFenced &&
    !view.settlementFenced &&
    !view.applyUncertain &&
    view.disposition == null
  );
}

export function codingWorktreeStatus(
  view: CodingWorktreeHostView | null,
): CodingWorktreeStatus {
  if (!view) return "none";
  if (view.phase === "destroyed") return "destroyed";
  if (view.disposition === "uncertain" || view.applyUncertain) return "uncertain";
  return view.phase;
}

export function codingWorktreeAffordances(
  view: CodingWorktreeHostView | null,
  applyTarget: string,
  expectedDigest: string,
): CodingWorktreeAffordances {
  if (!view) {
    return {
      status: "none",
      canPause: false,
      canStop: false,
      canAccept: false,
      canDiscard: false,
      canKeepForReview: false,
      reason:
        "No coding worktree session is attached. Pause, Stop, Accept, Discard, and Keep for review stay unavailable.",
    };
  }

  const status = codingWorktreeStatus(view);
  const settle = canSettle(view);
  const hasDigest = Boolean(view.patchDigest);
  const acceptReady =
    settle &&
    hasDigest &&
    applyTarget.trim().length > 0 &&
    expectedDigest.trim().length > 0;
  const closed = {
    canPause: false,
    canAccept: false,
    canDiscard: false,
    canKeepForReview: false,
  } as const;

  if (status === "destroyed") {
    return {
      status,
      ...closed,
      canStop: false,
      reason: "Destroyed: no further disposition commands.",
    };
  }

  if (status === "uncertain") {
    return {
      status,
      ...closed,
      canStop: true,
      reason:
        "Uncertain: apply may have partially happened. Auto-retry and auto-Accept are forbidden. Stop remains legal.",
    };
  }

  if (status === "paused") {
    return {
      status,
      ...closed,
      canStop: true,
      reason: "Paused: staging and settlement are fenced. Stop remains legal.",
    };
  }

  if (
    status === "settled" ||
    view.settlementFenced ||
    isSettledDisposition(view.disposition)
  ) {
    return {
      status,
      ...closed,
      canStop: true,
      reason: "Settled: Pause and settlement cannot be retried.",
    };
  }

  if (status === "stopped" || status === "stopping") {
    return {
      status,
      ...closed,
      canStop: true,
      reason:
        status === "stopped"
          ? "Stopped: destroy was not confirmed. Pause and settlement stay closed."
          : "Stopping: fence-first teardown is in progress.",
    };
  }

  return {
    status,
    canPause: settle,
    canStop: true,
    canAccept: acceptReady,
    canDiscard: settle,
    canKeepForReview: settle && hasDigest,
    reason: acceptReady
      ? "Active coding worktree session."
      : settle && hasDigest
        ? "Accept requires an explicit apply target and the exact sha256: digest. It is never auto-submitted."
        : settle
          ? "Accept and Keep for review require a staged patch digest. Discard and Pause remain available."
          : "Active coding worktree session.",
  };
}

function isSettledDisposition(disposition: CodingWorktreeDisposition | null): boolean {
  return (
    disposition === "accepted" ||
    disposition === "discarded" ||
    disposition === "kept_for_review"
  );
}
