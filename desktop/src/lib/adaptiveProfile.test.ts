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
      ceiling: "economy",
      generation: "a1b2c3d4e5f6",
      declaredCapabilityTrusted: false,
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
      maxVerificationFailures: 2,
    },
    escalations: [],
    cost: {
      providerAttempts: 2,
      acceptedAttempts: 2,
      failedAttempts: 0,
      observationBytes: 4096,
      screenshotBytes: 0,
      promptTokens: null,
      completionTokens: null,
    },
    stationaryRepeats: 0,
    observationTruncated: false,
    requiresIndependentVerifier: false,
    lifecycle: "idle",
    riskHighWater: "routine",
    revision: 2,
    terminal: null,
    ...overrides,
  };
}

describe("describeAdaptiveProfile", () => {
  it("shows unreported provider figures as unknown rather than zero", () => {
    const summary = describeAdaptiveProfile(projection());
    const tokens = summary.details.find((detail) => detail.label === "Tokens");
    expect(tokens?.value).toBe("unknown prompt / unknown completion");
    // Host-measured figures are known and must not read as unknown.
    expect(
      summary.details.find((detail) => detail.label === "Provider attempts")?.value,
    ).toBe("2 of 16");
    expect(
      summary.details.find((detail) => detail.label === "Observation sent")?.value,
    ).toBe("4.0 KB");
  });

  it("renders reported provider figures once a provider reports them", () => {
    const summary = describeAdaptiveProfile(
      projection({
        cost: {
          providerAttempts: 3,
          acceptedAttempts: 3,
          failedAttempts: 0,
          observationBytes: 2048,
          screenshotBytes: 0,
          promptTokens: 1234,
          completionTokens: 56,
        },
      }),
    );
    expect(
      summary.details.find((detail) => detail.label === "Tokens")?.value,
    ).toBe("1,234 prompt / 56 completion");
  });

  it("never presents a simulator pass as live eligibility", () => {
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
    const caution = summary.cautions.find((entry) => entry.code === "synthetic_only");
    expect(caution).toBeDefined();
    expect(caution?.message).toContain("deterministic simulator");
  });

  it("explains a bounded view as a reason to escalate rather than retry", () => {
    const summary = describeAdaptiveProfile(projection({ observationTruncated: true }));
    const caution = summary.cautions.find((entry) => entry.code === "bounded_view");
    expect(caution?.message).toContain("escalating is the fix");
  });

  it("names the required profile when a run stopped short of it", () => {
    const summary = describeAdaptiveProfile(
      projection({
        terminal: {
          kind: "stopped",
          reason: "independent_verifier_unavailable",
          message:
            "High Assurance requires a verifier independent of the proposing model, which is not available.",
          profile: "economy",
          requiredProfile: "high_assurance",
        },
      }),
    );
    expect(summary.ended?.title).toBe("Run stopped");
    expect(summary.ended?.message).toContain("It would have needed High Assurance.");
  });

  it("renders escalation history in order with canonical profile names", () => {
    const summary = describeAdaptiveProfile(
      projection({
        profile: "high_assurance",
        profileDisplayName: "High Assurance",
        escalations: [
          {
            from: "economy",
            to: "balanced",
            reason: "ambiguous_observation",
            message: "Several controls match the objective equally well.",
            revision: 3,
          },
          {
            from: "balanced",
            to: "high_assurance",
            reason: "repeated_stationarity",
            message: "The surface stopped changing.",
            revision: 5,
          },
        ],
      }),
    );
    expect(summary.escalations).toEqual([
      "Economy → Balanced: Several controls match the objective equally well.",
      "Balanced → High Assurance: The surface stopped changing.",
    ]);
  });

  it("never renders an alias for a canonical profile", () => {
    for (const profile of ["economy", "balanced", "high_assurance"] as const) {
      const summary = describeAdaptiveProfile(
        projection({ profile, profileDisplayName: "" }),
      );
      const serialized = JSON.stringify(summary);
      expect(serialized).not.toContain("efficient");
      expect(serialized).not.toContain("frontier");
    }
  });

  it("warns while the surface is not moving", () => {
    const summary = describeAdaptiveProfile(projection({ stationaryRepeats: 1 }));
    const caution = summary.cautions.find((entry) => entry.code === "stationary");
    expect(caution?.message).toContain("same actionable state 2 times");
  });

  it("says nothing about stationarity while the surface is moving", () => {
    const summary = describeAdaptiveProfile(projection());
    expect(summary.cautions.some((entry) => entry.code === "stationary")).toBe(false);
  });

  it("shows failed provider attempts alongside the total", () => {
    const summary = describeAdaptiveProfile(
      projection({
        cost: {
          providerAttempts: 5,
          acceptedAttempts: 2,
          failedAttempts: 3,
          observationBytes: 1024,
          screenshotBytes: 0,
          promptTokens: 900,
          completionTokens: null,
        },
      }),
    );
    expect(
      summary.details.find((detail) => detail.label === "Provider attempts")?.value,
    ).toBe("5 of 16 (3 failed)");
    // Usage billed by attempts that later failed is still usage.
    expect(
      summary.details.find((detail) => detail.label === "Tokens")?.value,
    ).toBe("900 prompt / unknown completion");
  });

  it("explains why a declared-only route may not act", () => {
    const base = projection();
    const summary = describeAdaptiveProfile({
      ...base,
      capability: {
        ...base.capability,
        attribution: "declared",
        declaredCapabilityTrusted: false,
      },
    });
    const caution = summary.cautions.find((entry) => entry.code === "declared_only");
    expect(caution?.message).toContain("declared, not measured");
  });

  it("surfaces an interrupted run as needing fresh authorization", () => {
    const summary = describeAdaptiveProfile(projection({ lifecycle: "interrupted" }));
    const caution = summary.cautions.find((entry) => entry.code === "interrupted");
    expect(caution?.message).toContain("Nothing was replayed");
  });

  it("shows the capability generation without any credential material", () => {
    const summary = describeAdaptiveProfile(projection());
    const detail = summary.details.find(
      (entry) => entry.label === "Capability generation",
    );
    expect(detail?.value).toBe("a1b2c3d4e5f6");
  });
});
