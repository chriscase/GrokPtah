import type {
  AdaptiveProfileProjection,
  AdaptiveProfileReason,
} from "./protocol";

export type AdaptiveProfileSummary = {
  profileName: string;
  reason: string;
  reasonCode: AdaptiveProfileReason;
  details: Array<{ label: string; value: string }>;
  escalations: string[];
  cautions: Array<{ code: string; message: string }>;
  ended: { title: string; message: string } | null;
};

const PROFILE_LABEL: Record<string, string> = {
  economy: "Economy",
  balanced: "Balanced",
  high_assurance: "High Assurance",
};

const DETAIL_LABEL: Record<string, string> = {
  semantic_only: "semantic structure",
  semantic_with_geometry: "semantic structure and geometry",
  semantic_with_evidence_ref: "semantic structure, geometry, and a redacted visual route",
};

const TERMINAL_LABEL: Record<string, string> = {
  completed: "Run completed",
  stopped: "Run stopped",
  interrupted: "Run interrupted by restart",
};

function profileLabel(value: string): string {
  return PROFILE_LABEL[value] ?? value;
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function optional(value: number | null | undefined): string {
  return typeof value === "number" ? value.toLocaleString() : "unknown";
}

export function describeAdaptiveProfile(
  projection: AdaptiveProfileProjection,
): AdaptiveProfileSummary {
  const { capability, budget, cost } = projection;
  const details = [
    {
      label: "Model sees",
      value: `${DETAIL_LABEL[budget.observationDetail] ?? budget.observationDetail}, up to ${budget.maxObservationElements} controls`,
    },
    {
      label: "Capability",
      value: `${capability.tier} (${capability.attribution})${
        capability.qualifiedVisualPath ? ", visual path qualified" : ", no visual path"
      }`,
    },
    { label: "Capability ceiling", value: profileLabel(capability.ceiling) },
    { label: "Task risk", value: projection.risk },
    { label: "Model calls", value: `${cost.modelCalls} of ${budget.maxModelCalls}` },
    { label: "Observation sent", value: formatBytes(cost.observationBytes) },
    {
      label: "Provider tokens",
      value: `${optional(cost.promptTokens)} prompt / ${optional(cost.completionTokens)} completion`,
    },
    {
      label: "Provider latency",
      value: cost.providerAttempts
        ? `${cost.providerLatencyMillis} ms across ${cost.providerAttempts} attempt(s)`
        : "unknown",
    },
  ];
  const cautions: Array<{ code: string; message: string }> = [];
  if (capability.syntheticOnly) {
    cautions.push({
      code: "synthetic_only",
      message:
        "This model is qualified only against deterministic evidence; that does not qualify it to drive a live application.",
    });
  }
  if (projection.observationTruncated) {
    cautions.push({
      code: "bounded_view",
      message:
        "The model saw a bounded view. If the needed control was not visible, the policy must escalate or stop rather than blindly retry.",
    });
  }
  if (capability.ceiling !== "high_assurance") {
    cautions.push({
      code: "capability_capped",
      message: `This route supports at most ${profileLabel(capability.ceiling)}; higher-risk work stops if stronger evidence is unavailable.`,
    });
  }
  if (projection.requiresIndependentVerifier && !capability.hostIndependentVerifier) {
    cautions.push({
      code: "independent_verifier_missing",
      message:
        "High Assurance requires an independent verifier, which is unavailable on this host.",
    });
  }
  if (projection.stationaryRepeats > 0) {
    cautions.push({
      code: "stationary",
      message: `The same actionable surface has appeared ${projection.stationaryRepeats + 1} times.`,
    });
  }
  const terminal = projection.terminal;
  return {
    profileName: profileLabel(projection.profile),
    reason: projection.message,
    reasonCode: projection.reason,
    details,
    escalations: projection.escalations.map(
      (entry) =>
        `${profileLabel(entry.from)} → ${profileLabel(entry.to)}: ${entry.message}`,
    ),
    cautions,
    ended: terminal
      ? {
          title: TERMINAL_LABEL[terminal.kind] ?? "Run ended",
          message: terminal.requiredProfile
            ? `${terminal.message} It would have needed ${profileLabel(terminal.requiredProfile)}.`
            : terminal.message,
        }
      : null,
  };
}
