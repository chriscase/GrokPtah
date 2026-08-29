import { describe, expect, it } from "vitest";
import { describeAdaptiveProfile } from "./adaptiveProfile";
import type { AdaptiveProfileProjection } from "./protocol";

function projection(
  overrides: Partial<AdaptiveProfileProjection> = {},
): AdaptiveProfileProjection {
  return {
    profile: "economy",
    profileDisplayName: "Economy",
    reason: "routine_task",
    message: "Routine task; the cheapest profile is sufficient.",
    risk: "routine",
    capability: {
      tier: "semantic_act",
      attribution: "measured",
      structuredTools: true,
      imageInput: false,
      qualifiedVisualPath: false,
      durableAuthority: true,
      sessionMeasured: false,
      syntheticOnly: false,
      hostScreenshotCapture: false,
      hostIndependentVerifier: false,
      hostIsolatedGuest: false,
      ceiling: "economy",
      capabilitySnapshotId: null,
    },
    budget: {
      observationDetail: "semantic_only",
      maxObservationElements: 48,
      maxObservationBytes: 24576,
      maxModelCalls: 16,
      maxRepairs: 1,
      maxTurnMillis: 20000,
      screenshotCaptureAllowed: false,
      pointerFallbackAllowed: false,
      keyChordAllowed: false,
    },
    safetyFloor: {
      requiresHostVerification: true,
      requiresFreshObservationBinding: true,
      requiresCompletionBoundToCurrentObservation: true,
      allowsScreenshotBytesToModel: false,
      allowsFreeFormAction: false,
      allowsAutomaticReplayAfterUncertainDispatch: false,
      maxStationaryRepeats: 2,
      maxConsecutiveUncertainAnswers: 2,
      minConfidencePermille: 700,
      maxVerificationFailures: 2,
    },
    escalations: [],
    cost: {
      modelCalls: 1,
      observationBytes: 2048,
      screenshotBytes: 0,
      providerAttempts: 1,
      providerLatencyMillis: 30,
      promptTokens: null,
      completionTokens: null,
    },
    stationaryRepeats: 0,
    observationTruncated: false,
    requiresIndependentVerifier: false,
    revision: 1,
    terminal: null,
    ...overrides,
  };
}

describe("describeAdaptiveProfile", () => {
  it("keeps provider usage unknown until a receipt reports it", () => {
    const summary = describeAdaptiveProfile(projection());
    expect(summary.details.find((detail) => detail.label === "Provider tokens")?.value).toBe(
      "unknown prompt / unknown completion",
    );
  });

  it("does not present synthetic qualification as live eligibility", () => {
    const summary = describeAdaptiveProfile(
      projection({
        capability: {
          ...projection().capability,
          durableAuthority: false,
          sessionMeasured: true,
          syntheticOnly: true,
        },
      }),
    );
    expect(summary.cautions.some((caution) => caution.code === "synthetic_only")).toBe(true);
  });

  it("explains a bounded view and terminal required profile", () => {
    const summary = describeAdaptiveProfile(
      projection({
        observationTruncated: true,
        terminal: {
          kind: "stopped",
          reason: "independent_verifier_unavailable",
          message: "High Assurance requires an independent verifier.",
          profile: "balanced",
          requiredProfile: "high_assurance",
        },
      }),
    );
    expect(summary.cautions.some((caution) => caution.code === "bounded_view")).toBe(true);
    expect(summary.ended?.message).toContain("High Assurance");
  });

  it("renders only canonical profile names in escalation copy", () => {
    const summary = describeAdaptiveProfile(
      projection({
        profile: "high_assurance",
        escalations: [
          {
            from: "economy",
            to: "balanced",
            reason: "ambiguous_observation",
            message: "More observation detail is required.",
            revision: 2,
          },
        ],
      }),
    );
    const encoded = JSON.stringify(summary);
    expect(encoded).not.toContain("efficient");
    expect(encoded).not.toContain("frontier");
    expect(summary.escalations[0]).toContain("Economy → Balanced");
  });
});
