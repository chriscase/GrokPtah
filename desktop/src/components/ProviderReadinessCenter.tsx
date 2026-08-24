import type {
  AdmissionEligibility,
  ComputerUseTier,
  NativeCodingReadinessProjection,
  ProviderModelSummary,
  ProviderProfileSummary,
  ProviderQualificationReport,
  QualificationCheck,
  QualificationEvidence,
} from "../lib/protocol";
import { sanitizeSensitiveText } from "../lib/errorMessage";
import "./ProviderReadinessCenter.css";

export const READINESS_VERDICT_LABEL = {
  ready: "Ready for native coding",
  eligible_unmeasured: "Eligible — not yet measured",
  requalify_required: "Requalification required",
  discussion_only: "Discussion only",
  credential_missing: "Credential missing",
  not_qualified: "Not qualified",
  configuration_incomplete: "Configuration incomplete",
} as const;

export const QUALIFICATION_EVIDENCE_LABEL = {
  measured: "Measured",
  declared: "Declared; not yet measured",
  stale: "Stale; requalification required",
  unknown: "Unknown",
} as const;

export type ReadinessVerdict = keyof typeof READINESS_VERDICT_LABEL;

export type ReadinessNextActionId =
  | "save_provider"
  | "discover_models"
  | "qualify_model"
  | "resolve_credentials"
  | "none";

export type ReadinessCheckStatus =
  | "pass"
  | "degraded"
  | "fail"
  | "unknown"
  | "declared"
  | "present"
  | "missing";

export type CapabilityProvenance = "measured" | "declared" | "unknown";

export type ReadinessCheckView = {
  id:
    | "credential"
    | "basic_generation"
    | "native_tool_call"
    | "tool_result_continuation"
    | "streaming"
    | "computer_use"
    | "capability_provenance";
  label: string;
  status: ReadinessCheckStatus;
  summary: string;
  detail: string;
  blocking: boolean;
};

export type ProviderReadinessInput = {
  providerId: string;
  providerLabel: string;
  baseUrl: string;
  modelId: string;
  profile: ProviderProfileSummary | null;
  report: ProviderQualificationReport | null;
  admission: NativeCodingReadinessProjection | null;
  busy: boolean;
  error: string | null;
  saveDisabled: boolean;
  discoverDisabled: boolean;
  qualifyDisabled: boolean;
};

export type ProviderReadinessView = {
  verdict: ReadinessVerdict;
  verdictLabel: string;
  tone: "ok" | "attention" | "danger" | "neutral";
  announce: "status" | "alert";
  summary: string;
  admissionLabel: string;
  evidenceLabel: string;
  qualificationEvidence: QualificationEvidence;
  managerProposalPermitted: boolean;
  managerProposalLabel: string;
  providerId: string;
  providerLabel: string;
  modelId: string;
  managedByEnv: boolean;
  hasSavedProfile: boolean;
  credentialSet: boolean;
  hasLiveReport: boolean;
  codingReady: boolean | null;
  codingProvenance: CapabilityProvenance;
  computerUse: {
    tier: ComputerUseTier;
    label: string;
    source: CapabilityProvenance;
    enabled: boolean;
    summary: string;
    observationDetail: string | null;
    staleRecoveryDetail: string | null;
  };
  checks: ReadinessCheckView[];
  blockingReasons: ReadinessCheckView[];
  streamingNote: string | null;
  nextAction: {
    id: ReadinessNextActionId;
    label: string;
    description: string;
    disabled: boolean;
  };
  error: string | null;
  busy: boolean;
};

export type ProviderReadinessCenterProps = ProviderReadinessInput & {
  onSaveProvider: () => void;
  onDiscoverModels: () => void;
  onQualifyModel: () => void;
  onResolveCredentials: () => void;
};

export function sanitizeReadinessText(text: string): string {
  return sanitizeSensitiveText(text);
}

export function computerUseTierLabel(tier: ComputerUseTier): string {
  switch (tier) {
    case "observe":
      return "Observation only";
    case "semantic_act":
      return "Semantic act";
    case "visual_fallback_act":
      return "Visual fallback act";
    default:
      return "None";
  }
}

export function selectModelSummary(
  profile: ProviderProfileSummary | null,
  modelId: string,
): ProviderModelSummary | null {
  if (!profile) return null;
  return profile.models.find((model) => model.id === modelId) ?? null;
}

function provenanceFrom(
  value: CapabilityProvenance | string | undefined,
): CapabilityProvenance {
  if (value === "measured" || value === "declared") return value;
  return "unknown";
}

function checkText(check: QualificationCheck): {
  status: ReadinessCheckStatus;
  summary: string;
  detail: string;
} {
  return {
    status: check.status,
    summary: sanitizeReadinessText(check.detail),
    detail: sanitizeReadinessText(`${check.status}: ${check.detail}`),
  };
}

function configurationComplete(input: ProviderReadinessInput): boolean {
  return (
    input.providerId.trim().length > 0 &&
    input.baseUrl.trim().length > 0 &&
    input.modelId.trim().length > 0
  );
}

function computerSummary(
  tier: ComputerUseTier,
  source: CapabilityProvenance,
): string {
  const independent =
    "Computer Use is independent of native coding readiness; coding tools never enable it.";
  if (source === "declared") {
    return `Declared Computer Use (${computerUseTierLabel(tier)}) is catalog metadata, not a measured qualification. ${independent}`;
  }
  if (source === "unknown") {
    return `Computer Use has not been measured for this exact model. ${independent}`;
  }
  switch (tier) {
    case "observe":
      return `Measured Computer Use tier: observation only. ${independent}`;
    case "semantic_act":
      return `Measured Computer Use tier: semantic act. ${independent}`;
    case "visual_fallback_act":
      return `Measured Computer Use tier: visual fallback act. ${independent}`;
    default:
      return `Measured Computer Use tier: none. ${independent}`;
  }
}

export function verdictFromEligibility(
  eligibility: AdmissionEligibility,
): ReadinessVerdict {
  switch (eligibility) {
    case "measured_eligible":
      return "ready";
    case "declared_unmeasured":
      return "eligible_unmeasured";
    case "requalify_required":
      return "requalify_required";
    case "discussion_only":
      return "discussion_only";
    case "credential_missing":
      return "credential_missing";
    case "configuration_incomplete":
      return "configuration_incomplete";
    default:
      return "not_qualified";
  }
}

function nextActionFor(
  verdict: ReadinessVerdict,
  input: ProviderReadinessInput,
  managedByEnv: boolean,
): ProviderReadinessView["nextAction"] {
  if (managedByEnv) {
    return {
      id: "none",
      label: "",
      description:
        "This profile is managed by environment variables. Readiness evidence is shown as-is; change those variables to edit credentials or re-qualify.",
      disabled: true,
    };
  }

  if (verdict === "configuration_incomplete" || !input.profile) {
    return {
      id: "save_provider",
      label: "Save provider",
      description:
        "Save this provider with an exact model ID before discovering models or qualifying native coding.",
      disabled: input.saveDisabled,
    };
  }

  if (verdict === "credential_missing") {
    return {
      id: "resolve_credentials",
      label: "Add provider key",
      description:
        "Store a provider API key on this profile, then save. Native coding stays blocked until a credential is present.",
      disabled: false,
    };
  }

  if (input.error) {
    return {
      id: "qualify_model",
      label: "Qualify model",
      description:
        "The last qualification failed. Retry this exact model; a second call is not started while the provider is busy.",
      disabled: input.qualifyDisabled || input.busy,
    };
  }

  if (input.profile.models.length === 0) {
    return {
      id: "discover_models",
      label: "Discover models",
      description:
        "Discover exact model IDs from this provider, then qualify the model you intend to use.",
      disabled: input.discoverDisabled,
    };
  }

  if (verdict === "ready") {
    return {
      id: "none",
      label: "",
      description:
        "Re-qualify from the provider actions if the endpoint, credential, or model changes.",
      disabled: true,
    };
  }

  return {
    id: "qualify_model",
    label: "Qualify model",
    description:
      verdict === "discussion_only"
        ? "Qualification already measured this exact model as discussion-only. Re-run only after the provider or model changes."
        : verdict === "eligible_unmeasured"
        ? "The host will currently admit native coding on this declared route. Qualify to measure the exact model."
        : verdict === "requalify_required"
          ? "Measured evidence is stale for this route or credential. Qualify this exact model before Execution."
          : "Qualify this exact model to measure native tool calls, continuation, streaming, and Computer Use.",
    disabled: input.qualifyDisabled,
  };
}

export function deriveProviderReadiness(
  input: ProviderReadinessInput,
): ProviderReadinessView {
  const profile = input.profile;
  const selectedModel = selectModelSummary(profile, input.modelId);
  const report = input.report;
  const admission =
    input.admission &&
    input.admission.providerId === input.providerId &&
    (input.admission.modelId === input.modelId ||
      input.admission.modelId.trim() === input.modelId.trim())
      ? input.admission
      : null;
  const managedByEnv = Boolean(admission?.managedByEnv ?? profile?.managedByEnv);
  const credentialSet = Boolean(
    admission?.credentialPresent ?? profile?.credentialSet,
  );
  const hasSavedProfile = profile !== null;
  const error = input.error ? sanitizeReadinessText(input.error) : null;
  const verdict = admission
    ? verdictFromEligibility(admission.execution.eligibility)
    : !configurationComplete(input) || !hasSavedProfile
      ? "configuration_incomplete"
      : "not_qualified";
  const qualificationEvidence: QualificationEvidence =
    admission?.qualificationEvidence ?? "unknown";
  const codingProvenance: CapabilityProvenance =
    qualificationEvidence === "stale" ? "declared" : qualificationEvidence;
  const computerSource: CapabilityProvenance = provenanceFrom(
    admission?.computerUse.source ?? selectedModel?.computerCapabilitySource,
  );
  const computerTier: ComputerUseTier =
    admission?.computerUse.tier ??
    report?.computerUseTier ??
    selectedModel?.computerUseTier ??
    "none";
  const computerEnabled = Boolean(admission?.computerUse.enabled);

  const credentialCheck: ReadinessCheckView = {
    id: "credential",
    label: "Credential",
    status: credentialSet ? "present" : "missing",
    summary: credentialSet
      ? managedByEnv
        ? "An environment-managed credential is present."
        : "A provider credential is stored."
      : hasSavedProfile
        ? "No provider credential is stored for this profile."
        : "Save the provider to store a credential.",
    detail: credentialSet
      ? "credentialSet=true"
      : "credentialSet=false",
    blocking: verdict === "credential_missing",
  };

  const basic: ReadinessCheckView = report
    ? {
        id: "basic_generation",
        label: "Basic generation",
        ...checkText(report.basicGeneration),
        blocking: report.basicGeneration.status === "fail" && verdict !== "ready",
      }
    : {
        id: "basic_generation",
        label: "Basic generation",
        status: codingProvenance === "declared" ? "declared" : "unknown",
        summary:
          codingProvenance === "declared"
            ? "Catalog declaration only; basic generation has not been measured."
            : "Not measured for this exact model yet.",
        detail: "No qualification report for basic generation.",
        blocking: false,
      };

  let native: ReadinessCheckView;
  if (report) {
    native = {
      id: "native_tool_call",
      label: "Native tool calls",
      ...checkText(report.nativeToolCall),
      blocking: report.nativeToolCall.status !== "pass" && verdict === "discussion_only",
    };
  } else if (codingProvenance === "declared") {
    native = {
      id: "native_tool_call",
      label: "Native tool calls",
      status: "declared",
      summary: selectedModel?.supportsTools
        ? "The catalog declares native tools. Qualification evidence is still unmeasured."
        : "The catalog does not declare native tools, and nothing has been measured.",
      detail: `declared supportsTools=${String(selectedModel?.supportsTools ?? false)}`,
      blocking: verdict === "requalify_required" || verdict === "not_qualified",
    };
  } else if (codingProvenance === "measured") {
    native = {
      id: "native_tool_call",
      label: "Native tool calls",
      status: selectedModel?.supportsTools ? "pass" : "fail",
      summary: selectedModel?.supportsTools
        ? "Measured native tool calls are available."
        : "Measured as chat-only; native tool calls are not available.",
      detail: `measured supportsTools=${String(selectedModel?.supportsTools ?? false)}`,
      blocking: !selectedModel?.supportsTools,
    };
  } else {
    native = {
      id: "native_tool_call",
      label: "Native tool calls",
      status: "unknown",
      summary: "Native tool calls have not been qualified for this exact model.",
      detail: "No qualification report for native tool calls.",
      blocking: verdict === "not_qualified",
    };
  }

  const continuation: ReadinessCheckView = report
    ? {
        id: "tool_result_continuation",
        label: "Tool-result continuation",
        ...checkText(report.toolResultContinuation),
        blocking:
          report.toolResultContinuation.status !== "pass" &&
          verdict === "discussion_only",
      }
    : {
        id: "tool_result_continuation",
        label: "Tool-result continuation",
        status: "unknown",
        summary:
          "Tool-result continuation is not stored on the model snapshot. Qualify this exact model to measure it.",
        detail: "No qualification report for tool-result continuation.",
        blocking: false,
      };

  let streaming: ReadinessCheckView;
  if (report) {
    streaming = {
      id: "streaming",
      label: "Streaming",
      ...checkText(report.streaming),
      blocking: false,
    };
  } else if (codingProvenance === "declared") {
    streaming = {
      id: "streaming",
      label: "Streaming",
      status: "declared",
      summary: selectedModel?.supportsStream
        ? "The catalog declares streaming. That is not a measured qualification."
        : "The catalog does not declare streaming, and nothing has been measured.",
      detail: `declared supportsStream=${String(selectedModel?.supportsStream ?? false)}`,
      blocking: false,
    };
  } else if (codingProvenance === "measured") {
    streaming = {
      id: "streaming",
      label: "Streaming",
      status: selectedModel?.supportsStream ? "pass" : "degraded",
      summary: selectedModel?.supportsStream
        ? "Measured streaming is available."
        : "Streaming was not recorded as available. This does not replace measured tool readiness.",
      detail: `measured supportsStream=${String(selectedModel?.supportsStream ?? false)}`,
      blocking: false,
    };
  } else {
    streaming = {
      id: "streaming",
      label: "Streaming",
      status: "unknown",
      summary: "Streaming has not been qualified for this exact model.",
      detail: "No qualification report for streaming.",
      blocking: false,
    };
  }

  const computerCheck: ReadinessCheckView = {
    id: "computer_use",
    label: "Computer Use",
    status:
      computerSource === "declared"
        ? "declared"
        : computerSource === "unknown"
          ? "unknown"
          : computerTier === "none"
            ? "fail"
            : "pass",
    summary: computerSummary(computerTier, computerSource),
    detail: `tier=${computerTier} source=${computerSource}`,
    blocking: false,
  };

  const provenanceCheck: ReadinessCheckView = {
    id: "capability_provenance",
    label: "Capability provenance",
    status:
      codingProvenance === "measured"
        ? "pass"
        : codingProvenance === "declared"
          ? "declared"
          : "unknown",
    summary:
      qualificationEvidence === "measured"
        ? "Qualification evidence is measured for this exact model."
        : qualificationEvidence === "declared"
          ? "Qualification evidence is declared, not measured. Admission eligibility is decided by the host."
          : qualificationEvidence === "stale"
            ? "Measured evidence exists for this model, but the current route or credential is declared and requires requalification."
            : "Qualification evidence is unknown until this exact model is qualified.",
    detail: `coding=${qualificationEvidence} computer=${computerSource}`,
    blocking: verdict === "requalify_required" || verdict === "not_qualified",
  };

  const checks = [
    credentialCheck,
    basic,
    native,
    continuation,
    streaming,
    computerCheck,
    provenanceCheck,
  ];

  const blockingReasons = checks.filter((check) => check.blocking);

  let summary: string;
  switch (verdict) {
    case "configuration_incomplete":
      summary = hasSavedProfile
        ? "Enter a provider ID, base URL, and exact model ID before measuring native coding readiness."
        : "Save this provider profile with an exact model ID. Readiness is computed only from a saved profile and that model.";
      break;
    case "credential_missing":
      summary =
        codingProvenance === "measured"
          ? "This exact model has measured native-coding evidence, but no provider credential is stored. Native coding stays blocked until a credential is saved."
          : "No provider credential is stored. Native coding stays blocked until a credential is saved.";
      break;
    case "not_qualified":
      summary =
        "This exact model is unknown to the host. Autonomous Execution and ManagerProposal stay refused until it is declared or measured.";
      break;
    case "eligible_unmeasured":
      summary =
        "The host will currently admit native coding on this declared first-use route. Qualification evidence has not been measured yet. Computer Use is listed separately and is not implied.";
      break;
    case "requalify_required":
      summary =
        "Measured qualification history exists for this model, but the current route or credential fell back to declared capabilities. Execution is blocked until this exact route is requalified. ManagerProposal may still use chat.";
      break;
    case "discussion_only": {
      const reasons = blockingReasons
        .filter((item) => item.id !== "credential")
        .map((item) => `${item.label}: ${item.summary}`);
      summary =
        reasons.length > 0
          ? `This exact model can be used for discussion, not the native coding loop. ${reasons.join(" ")}`
          : "This exact model can be used for discussion, not the native coding loop.";
      break;
    }
    case "ready":
      summary =
        "Measured native tool calls and coding readiness are present for this exact model. Computer Use is listed separately and is not implied.";
      break;
  }

  const streamingNote =
    streaming.status === "degraded" || streaming.status === "fail"
      ? verdict === "ready"
        ? `Streaming is degraded: ${streaming.summary} Native coding readiness is unchanged.`
        : `Streaming: ${streaming.summary}`
      : null;

  const tone =
    verdict === "ready"
      ? "ok"
      : verdict === "credential_missing"
        ? "danger"
        : verdict === "discussion_only" ||
            verdict === "not_qualified" ||
            verdict === "requalify_required" ||
            verdict === "eligible_unmeasured"
          ? "attention"
          : "neutral";

  const managerProposalPermitted = Boolean(
    admission?.managerProposal.permitted,
  );

  return {
    verdict,
    verdictLabel: READINESS_VERDICT_LABEL[verdict],
    tone,
    announce: verdict === "credential_missing" ? "alert" : "status",
    summary,
    admissionLabel: READINESS_VERDICT_LABEL[verdict],
    evidenceLabel: QUALIFICATION_EVIDENCE_LABEL[qualificationEvidence],
    qualificationEvidence,
    managerProposalPermitted,
    managerProposalLabel: managerProposalPermitted
      ? "Eligible for ManagerProposal (chat only; tools host-denied)"
      : "Not eligible for ManagerProposal",
    providerId: input.providerId,
    providerLabel: input.providerLabel,
    modelId: input.modelId,
    managedByEnv,
    hasSavedProfile,
    credentialSet,
    hasLiveReport: Boolean(report),
    codingReady: report ? report.codingReady : null,
    codingProvenance,
    computerUse: {
      tier: computerTier,
      label: computerUseTierLabel(computerTier),
      source: computerSource,
      enabled: computerEnabled,
      summary: computerSummary(computerTier, computerSource),
      observationDetail: report
        ? sanitizeReadinessText(
            `${report.semanticObservation.status}: ${report.semanticObservation.detail}`,
          )
        : null,
      staleRecoveryDetail: report
        ? sanitizeReadinessText(
            `${report.staleObservationRecovery.status}: ${report.staleObservationRecovery.detail}`,
          )
        : null,
    },
    checks,
    blockingReasons,
    streamingNote,
    nextAction: nextActionFor(verdict, input, managedByEnv),
    error,
    busy: input.busy,
  };
}

export function ProviderReadinessCenter(props: ProviderReadinessCenterProps) {
  const view = deriveProviderReadiness(props);
  const headingId = "provider-readiness-heading";
  const subject =
    view.modelId.trim().length > 0
      ? view.modelId
      : "no model selected";

  function runNextAction() {
    if (view.nextAction.disabled || props.busy) return;
    switch (view.nextAction.id) {
      case "save_provider":
        props.onSaveProvider();
        return;
      case "discover_models":
        props.onDiscoverModels();
        return;
      case "qualify_model":
        props.onQualifyModel();
        return;
      case "resolve_credentials":
        props.onResolveCredentials();
        return;
      default:
        return;
    }
  }

  return (
    <section
      className="provider-readiness"
      data-testid="native-coding-readiness"
      data-verdict={view.verdict}
      data-admission={view.verdict}
      data-evidence={view.qualificationEvidence}
      data-computer-tier={view.computerUse.tier}
      data-computer-enabled={String(view.computerUse.enabled)}
      aria-labelledby={headingId}
    >
      <div className="provider-readiness-header">
        <h3 className="provider-readiness-kicker" id={headingId}>
          Native coding readiness
        </h3>
        <p className="provider-readiness-subject">
          Provider <code>{view.providerId || "unsaved"}</code>
          {view.providerLabel && view.providerLabel !== view.providerId
            ? ` · ${view.providerLabel}`
            : ""}
          {" · model "}
          <code data-testid="readiness-model-id">{subject}</code>
        </p>
      </div>

      <div
        className={`provider-readiness-banner is-${view.tone}`}
        data-testid="readiness-verdict"
      >
        <p
          className="provider-readiness-verdict"
          role={view.announce}
          aria-live={view.announce === "status" ? "polite" : undefined}
        >
          {view.verdictLabel}
        </p>
        <p className="provider-readiness-summary">{view.summary}</p>
      </div>

      <div className="provider-readiness-axes">
        <div data-testid="readiness-admission-eligibility">
          <p className="provider-readiness-reasons-label">Admission eligibility</p>
          <p className="provider-readiness-axis-value">{view.admissionLabel}</p>
        </div>
        <div data-testid="readiness-qualification-evidence">
          <p className="provider-readiness-reasons-label">Qualification evidence</p>
          <p className="provider-readiness-axis-value">{view.evidenceLabel}</p>
        </div>
      </div>

      <div
        className="provider-readiness-manager"
        data-testid="readiness-manager-proposal"
      >
        <p className="provider-readiness-reasons-label">ManagerProposal</p>
        <p className="provider-readiness-axis-value">
          {view.managerProposalLabel}
        </p>
      </div>

      {view.busy && (
        <p className="provider-readiness-busy" role="status">
          Updating provider readiness. Qualification is not started again
          automatically.
        </p>
      )}

      {view.error && (
        <p className="provider-readiness-error" role="alert">
          {view.error}
        </p>
      )}

      {view.blockingReasons.length > 0 && (
        <div>
          <p className="provider-readiness-reasons-label" id="readiness-blockers">
            Blocking reasons
          </p>
          <ul
            className="provider-readiness-reasons"
            data-testid="readiness-blocking-reasons"
            aria-labelledby="readiness-blockers"
          >
            {view.blockingReasons.map((reason) => (
              <li className="provider-readiness-reason" key={reason.id}>
                <span className="provider-readiness-reason-label">
                  {reason.label}
                </span>
                <span className="provider-readiness-reason-text">
                  {reason.summary}
                </span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {view.streamingNote && (
        <p className="provider-readiness-note" data-testid="readiness-streaming-note">
          {view.streamingNote}
        </p>
      )}

      <div
        className="provider-readiness-computer"
        data-testid="readiness-computer-tier"
      >
        <p className="provider-readiness-computer-label">Computer Use</p>
        <p className="provider-readiness-computer-tier">
          {view.computerUse.label}
        </p>
        <p className="provider-readiness-computer-copy">
          {view.computerUse.summary}
        </p>
      </div>

      {view.managedByEnv && (
        <p className="provider-readiness-env" data-testid="readiness-env">
          This profile is managed by environment variables. The evidence above
          is still the current readiness for this exact model.
        </p>
      )}

      <div className="provider-readiness-action">
        <p className="provider-readiness-action-label">Next safe action</p>
        {view.nextAction.id !== "none" ? (
          <button
            type="button"
            className="primary"
            data-testid="readiness-next-action"
            disabled={view.nextAction.disabled || view.busy}
            onClick={runNextAction}
          >
            {view.nextAction.label}
          </button>
        ) : (
          <span data-testid="readiness-next-action-none">None required</span>
        )}
        <p className="provider-readiness-action-copy">{view.nextAction.description}</p>
      </div>

      <details className="provider-readiness-details" data-testid="readiness-details">
        <summary>Power-user evidence</summary>
        <div className="provider-readiness-evidence">
          <dl className="provider-readiness-dl">
            <dt>Exact model ID</dt>
            <dd>
              <code>{view.modelId || "(none)"}</code>
            </dd>
            <dt>Provider ID</dt>
            <dd>
              <code>{view.providerId || "(none)"}</code>
            </dd>
            <dt>Admission eligibility</dt>
            <dd>{view.admissionLabel}</dd>
            <dt>Qualification evidence</dt>
            <dd>{view.qualificationEvidence}</dd>
            <dt>ManagerProposal</dt>
            <dd>{view.managerProposalPermitted ? "eligible" : "refused"}</dd>
            <dt>Live report</dt>
            <dd>{view.hasLiveReport ? "yes" : "no"}</dd>
            <dt>codingReady</dt>
            <dd>
              {view.codingReady === null ? "not in this snapshot" : String(view.codingReady)}
            </dd>
            <dt>Coding provenance</dt>
            <dd>{view.codingProvenance}</dd>
            <dt>Computer provenance</dt>
            <dd>{view.computerUse.source}</dd>
            <dt>Computer tier</dt>
            <dd>{view.computerUse.tier}</dd>
            {view.computerUse.observationDetail && (
              <>
                <dt>Semantic observation</dt>
                <dd>{view.computerUse.observationDetail}</dd>
              </>
            )}
            {view.computerUse.staleRecoveryDetail && (
              <>
                <dt>Stale-observation recovery</dt>
                <dd>{view.computerUse.staleRecoveryDetail}</dd>
              </>
            )}
          </dl>
          <ul className="provider-readiness-checks" data-testid="readiness-checks">
            {view.checks.map((check) => (
              <li className="provider-readiness-check" key={check.id}>
                <span className="provider-readiness-check-label">
                  {check.label}
                </span>
                <span className="provider-readiness-check-text">
                  {check.status}: {check.detail}
                </span>
              </li>
            ))}
          </ul>
        </div>
      </details>
    </section>
  );
}
