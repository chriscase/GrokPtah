/**
 * Operator-readable rendering of the adaptive Computer Use profile (#435).
 *
 * Pure and free of React so the cockpit and its tests read the same logic. It
 * only formats what the Rust projection already decided; it never re-derives a
 * profile, a reason, or a cost, because two surfaces deriving their own view is
 * how a GUI and an external observer come to disagree.
 *
 * The one rule this module enforces on its own is about *unknown*. A provider
 * figure the projection reports as `null` renders as "unknown", never as zero
 * and never as an estimate.
 */
import type {
  AdaptiveProfileProjection,
  AdaptiveProfileReason,
} from "./protocol";

/** One labelled fact for the cockpit's profile panel. */
export type AdaptiveDetail = {
  label: string;
  value: string;
};

/** A caution the operator should read before approving anything. */
export type AdaptiveCaution = {
  code:
    | "synthetic_only"
    | "bounded_view"
    | "capability_capped"
    | "independent_verifier_missing"
    | "declared_only"
    | "interrupted"
    | "stationary";
  message: string;
};

export type AdaptiveProfileSummary = {
  /** e.g. "Economy". Always a canonical profile, never an alias. */
  profileName: string;
  /** Why this profile is in force, in the operator's words. */
  reason: string;
  reasonCode: AdaptiveProfileReason;
  details: AdaptiveDetail[];
  escalations: string[];
  cautions: AdaptiveCaution[];
  /** Present once the run has ended, whatever the outcome. */
  ended: { title: string; message: string } | null;
};

const OBSERVATION_DETAIL_LABEL: Record<string, string> = {
  semantic_only: "semantics only",
  semantic_with_geometry: "semantics and geometry",
  semantic_with_evidence_ref: "semantics, geometry, and a redacted capture reference",
};

const TERMINAL_TITLE: Record<string, string> = {
  completed: "Run completed",
  stopped: "Run stopped",
  interrupted: "Run interrupted by restart",
};

const PROFILE_LABEL: Record<string, string> = {
  economy: "Economy",
  balanced: "Balanced",
  high_assurance: "High Assurance",
};

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Renders a provider-reported figure. `null` means the provider reported
 * nothing, which is a fact worth showing rather than a zero worth inventing.
 */
function formatOptional(value: number | null | undefined): string {
  return typeof value === "number" ? value.toLocaleString() : "unknown";
}

function profileLabel(profile: string): string {
  return PROFILE_LABEL[profile] ?? profile;
}

export function describeAdaptiveProfile(
  projection: AdaptiveProfileProjection,
): AdaptiveProfileSummary {
  const { capability, budget, cost } = projection;

  const details: AdaptiveDetail[] = [
    {
      label: "Model sees",
      value: `${OBSERVATION_DETAIL_LABEL[budget.observationDetail] ?? budget.observationDetail}, up to ${budget.maxObservationElements} controls`,
    },
    {
      label: "Capability",
      value: `${capability.tier} (${capability.attribution})${
        capability.qualifiedVisualPath ? ", visual path qualified" : ", no visual path"
      }`,
    },
    { label: "Ceiling", value: profileLabel(capability.ceiling) },
    { label: "Task risk", value: projection.risk },
    {
      label: "Provider attempts",
      value: `${cost.providerAttempts} of ${budget.maxModelCalls}${
        cost.failedAttempts > 0 ? ` (${cost.failedAttempts} failed)` : ""
      }`,
    },
    { label: "Observation sent", value: formatBytes(cost.observationBytes) },
    {
      label: "Tokens",
      value: `${formatOptional(cost.promptTokens)} prompt / ${formatOptional(
        cost.completionTokens,
      )} completion`,
    },
    { label: "Screenshot bytes sent", value: formatBytes(cost.screenshotBytes) },
    { label: "Capability generation", value: capability.generation },
  ];

  const escalations = projection.escalations.map(
    (escalation) =>
      `${profileLabel(escalation.from)} → ${profileLabel(escalation.to)}: ${escalation.message}`,
  );

  const cautions: AdaptiveCaution[] = [];
  if (capability.syntheticOnly) {
    cautions.push({
      code: "synthetic_only",
      message:
        "This model is qualified only against the deterministic simulator. That is not evidence it can drive a real application, so it is held to the cheapest profile.",
    });
  }
  if (projection.observationTruncated) {
    cautions.push({
      code: "bounded_view",
      message:
        "This profile showed the model a bounded set of controls. If the right control was not among them, escalating is the fix — not retrying.",
    });
  }
  if (capability.ceiling !== "high_assurance") {
    cautions.push({
      code: "capability_capped",
      message: `This model and host can support at most ${profileLabel(
        capability.ceiling,
      )}. Work needing more assurance will stop rather than run under a profile it has not earned.`,
    });
  }
  if (projection.requiresIndependentVerifier && !capability.hostIndependentVerifier) {
    cautions.push({
      code: "independent_verifier_missing",
      message:
        "High Assurance requires a verifier independent of the proposing model, which this build does not have.",
    });
  }
  if (!capability.declaredCapabilityTrusted && capability.attribution === "declared") {
    cautions.push({
      code: "declared_only",
      message:
        "This route's Computer capability is declared, not measured, and local policy does not trust declared capability for action. It may observe only.",
    });
  }
  if (projection.lifecycle === "interrupted") {
    cautions.push({
      code: "interrupted",
      message:
        "A restart cut this run mid-turn. Nothing was replayed; a fresh authorization is required.",
    });
  }
  if (projection.stationaryRepeats > 0) {
    cautions.push({
      code: "stationary",
      message: `The surface has presented the same actionable state ${
        projection.stationaryRepeats + 1
      } times. Repeating the last action would not be progress.`,
    });
  }

  const terminal = projection.terminal ?? null;

  return {
    profileName: projection.profileDisplayName || profileLabel(projection.profile),
    reason: projection.message,
    reasonCode: projection.reason,
    details,
    escalations,
    cautions,
    ended: terminal
      ? {
          title: TERMINAL_TITLE[terminal.kind] ?? "Run ended",
          message: terminal.requiredProfile
            ? `${terminal.message} It would have needed ${profileLabel(terminal.requiredProfile)}.`
            : terminal.message,
        }
      : null,
  };
}
