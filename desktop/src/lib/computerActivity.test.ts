import { describe, expect, it } from "vitest";
import type { ComputerRunProjection } from "./protocol";
import { computerActivityAnnouncement, computerActivityState } from "./computerActivity";

function projection(
  overrides: Partial<ComputerRunProjection> = {},
): ComputerRunProjection {
  return {
    runId: "run-1",
    ownerSessionId: "11111111-1111-4111-8111-111111111111",
    parentRunId: null,
    campaignId: null,
    target: {
      appId: "com.grokptah.demo",
      windowId: "main",
      generation: 1,
      displayName: "Demo",
      sensitivity: "none",
    },
    state: "ready",
    controlDisposition: "agent_owned",
    controlEpoch: 0,
    version: 2,
    agentActive: false,
    terminal: false,
    createdAt: "2026-08-14T00:00:00Z",
    updatedAt: "2026-08-14T00:00:01Z",
    startedAt: "2026-08-14T00:00:01Z",
    endedAt: null,
    progress: {
      actionCount: 0,
      maxActions: 64,
      evidenceBytes: 0,
      maxEvidenceBytes: 1024,
      elapsedMillis: 1000,
      maxDurationSecs: 900,
      durationExceeded: false,
    },
    grant: null,
    observation: null,
    lastOutcome: null,
    lastError: null,
    eventRange: null,
    ...overrides,
  };
}

describe("computerActivityState", () => {
  it("distinguishes an actively working agent from an idle agent-owned run", () => {
    expect(computerActivityState(projection({ agentActive: true })).id).toBe("agent_active");
    expect(computerActivityState(projection({ agentActive: false })).id).toBe("agent_idle");
  });

  it("renders every durable control disposition as its own visible state", () => {
    const seen = new Map<string, string>();
    for (const disposition of [
      "agent_owned",
      "paused",
      "operator_takeover",
      "stopped",
      "interrupted",
      "uncertain_outcome",
    ] as const) {
      const state = computerActivityState(
        projection({
          controlDisposition: disposition,
          // Keep the record valid: these dispositions imply non-agent control.
          terminal: disposition === "stopped" || disposition === "interrupted",
        }),
      );
      expect(state.label).not.toBe("");
      seen.set(disposition, state.id);
    }
    // No two dispositions may collapse into the same visible state.
    expect(new Set(seen.values()).size).toBe(seen.size);
  });

  it("never presents takeover or stop as an ordinary pause", () => {
    // All three share the paused lifecycle state; only the disposition differs.
    const paused = computerActivityState(
      projection({ state: "paused", controlDisposition: "paused" }),
    );
    const takeover = computerActivityState(
      projection({ state: "paused", controlDisposition: "operator_takeover" }),
    );
    const stopped = computerActivityState(
      projection({ state: "cancelled", controlDisposition: "stopped", terminal: true }),
    );

    expect(paused.id).toBe("paused");
    expect(paused.absorbing).toBe(false);
    expect(takeover.id).toBe("operator_takeover");
    expect(takeover.absorbing).toBe(true);
    expect(stopped.id).toBe("stopped");
    expect(stopped.absorbing).toBe(true);
  });

  it("keeps an active agent controllable so Stop stays reachable", () => {
    expect(computerActivityState(projection({ agentActive: true })).controllable).toBe(true);
    expect(
      computerActivityState(projection({ controlDisposition: "paused" })).controllable,
    ).toBe(true);
  });

  it("treats a late uncertain outcome as needing attention, not success", () => {
    const state = computerActivityState(
      projection({
        controlDisposition: "uncertain_outcome",
        state: "failed",
        terminal: true,
        lastError: { code: "uncertain_outcome", message: "action completed after cancel" },
      }),
    );
    expect(state.id).toBe("uncertain_outcome");
    expect(state.tone).toBe("attention");
    expect(state.absorbing).toBe(true);
    expect(state.detail).toMatch(/unknown/i);
  });

  it("shows restart interruption as attention rather than a normal ending", () => {
    const state = computerActivityState(
      projection({
        controlDisposition: "interrupted",
        state: "interrupted",
        terminal: true,
      }),
    );
    expect(state.id).toBe("interrupted");
    expect(state.tone).toBe("attention");
    expect(state.detail).toMatch(/not resume|nothing resumes/i);
  });

  it("does not present a terminal agent-owned run as live agent control", () => {
    const state = computerActivityState(
      projection({
        controlDisposition: "agent_owned",
        state: "completed",
        terminal: true,
        agentActive: false,
      }),
    );
    expect(state.id).toBe("stopped");
    expect(state.controllable).toBe(false);
    expect(state.label).toMatch(/completed/i);
  });

  it("fails closed on a control disposition it does not recognize", () => {
    const state = computerActivityState(
      projection({ controlDisposition: "some_future_state" }),
    );
    expect(state.tone).toBe("attention");
    expect(state.controllable).toBe(false);
    expect(state.absorbing).toBe(true);
    expect(state.label).toMatch(/unrecognized/i);
  });
});

describe("computerActivityAnnouncement", () => {
  it("names the state and the target for assistive technology", () => {
    const announcement = computerActivityAnnouncement(
      projection({ agentActive: true, target: { ...projection().target, displayName: "Notes" } }),
    );
    expect(announcement).toMatch(/Agent is using the computer/);
    expect(announcement).toMatch(/Target Notes/);
  });
});
