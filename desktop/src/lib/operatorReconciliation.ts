/**
 * Operator reconciliation handoff for durable always-on runs.
 *
 * This module is the desktop, CLI, and API-side mirror of the Rust
 * `grokptah_agent_sdk::reconciliation` contract. It is deliberately
 * transport-neutral and side-effect free: it parses an authority's projection
 * and *builds* a reconciliation request, but it never sends one. The caller
 * hands the built payload to whichever client it already holds, which is what
 * keeps a self-hosted coding-agent operator, the cockpit, and a CLI on one
 * code path without this file growing a network dependency.
 *
 * The contract it mirrors has one hard property, and this module preserves it:
 * no reconciliation action can resend, retry, or otherwise mutate a provider
 * attempt. {@link RECONCILE_ACTIONS} is the closed set, and
 * {@link buildReconcileRequest} rejects anything outside it.
 */

export const RECONCILIATION_CONTRACT = "grokptah.operator-reconciliation.v1" as const;

/** Tool that applies one operator reconciliation intent. */
export const RECONCILE_TOOL = "ptah_reconcile_run" as const;
/** Read-only tool that pages a run's reconciliation history. */
export const RECONCILE_HISTORY_TOOL = "ptah_get_reconciliation_history" as const;

/** Maximum UTF-8 bytes in a redacted operator note. */
export const MAX_NOTE_BYTES = 2_048;
/** Maximum UTF-8 bytes in one redacted evidence summary. */
export const MAX_EVIDENCE_SUMMARY_BYTES = 512;
/** Maximum evidence records accepted in one reconciliation request. */
export const MAX_EVIDENCE_PER_REQUEST = 16;
/** Maximum entries returned by one history page. */
export const MAX_HISTORY_PAGE = 64;

export type UncertaintyDomain =
  | "model_or_provider"
  | "worker_or_lease"
  | "operator_decision";

export type RunConfidence = "confirmed" | "unconfirmed" | "uncertain";

export type AttentionSeverity = "blocking" | "degraded" | "advisory";

export type AttentionReason =
  | "uncertain_outcome"
  | "crash_recovered"
  | "lease_expired"
  | "provider_ambiguity"
  | "cancel_unconfirmed"
  | "deadline_exceeded"
  | "stream_gap"
  | "stale_observation";

export type DurableRunState =
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted"
  | "limit_reached";

/**
 * The closed set of operator actions.
 *
 * Ordered least- to most-asserting. There is no resend, retry, or resume
 * member, and adding one here would not be enough: the authority's own enum
 * is the boundary that actually enforces it.
 */
export const RECONCILE_ACTIONS = [
  "record_evidence",
  "acknowledge",
  "resolve_completed",
  "resolve_failed",
  "resolve_cancelled",
] as const;

export type ReconcileAction = (typeof RECONCILE_ACTIONS)[number];

export type EvidenceKind =
  | "provider_projection"
  | "host_journal"
  | "workspace_inspection"
  | "operator_statement";

export type EvidenceRecord = {
  kind: EvidenceKind;
  /** Content digest of the underlying material, never the material itself. */
  digest: string;
  /** Redacted, bounded human summary. */
  summary: string;
};

export type RunAttention = {
  contract: typeof RECONCILIATION_CONTRACT;
  runRef: string;
  state: DurableRunState;
  confidence: RunConfidence;
  needsAttention: boolean;
  reasons: AttentionReason[];
  severity?: AttentionSeverity;
  domains: UncertaintyDomain[];
  observedSeq: number;
  revision: number;
};

export type ReconciliationScope = {
  sessionId: string;
  workspace: string;
  runId: string;
};

export type OperatorIdentity = {
  operatorRef: string;
  authorityRef: string;
};

export type ReconcileRequestInput = {
  requestId: string;
  scope: ReconciliationScope;
  /** The revision the operator actually looked at. */
  expectedRevision: number;
  action: ReconcileAction;
  evidence?: EvidenceRecord[];
  note?: string;
  operator: OperatorIdentity;
};

/** Raised when an operator surface builds or receives a malformed payload. */
export class ReconciliationError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ReconciliationError";
  }
}

const REASONS: ReadonlySet<AttentionReason> = new Set([
  "uncertain_outcome",
  "crash_recovered",
  "lease_expired",
  "provider_ambiguity",
  "cancel_unconfirmed",
  "deadline_exceeded",
  "stream_gap",
  "stale_observation",
]);

const DOMAINS: ReadonlySet<UncertaintyDomain> = new Set([
  "model_or_provider",
  "worker_or_lease",
  "operator_decision",
]);

const CONFIDENCES: ReadonlySet<RunConfidence> = new Set([
  "confirmed",
  "unconfirmed",
  "uncertain",
]);

const SEVERITIES: ReadonlySet<AttentionSeverity> = new Set([
  "blocking",
  "degraded",
  "advisory",
]);

const RUN_STATES: ReadonlySet<DurableRunState> = new Set([
  "queued",
  "running",
  "completed",
  "failed",
  "cancelled",
  "interrupted",
  "limit_reached",
]);

const EVIDENCE_KINDS: ReadonlySet<EvidenceKind> = new Set([
  "provider_projection",
  "host_journal",
  "workspace_inspection",
  "operator_statement",
]);

/** Severity order used wherever attention has to be ranked. */
const SEVERITY_RANK: Record<AttentionSeverity, number> = {
  blocking: 0,
  degraded: 1,
  advisory: 2,
};

/** Reason order, matching the authority's own declaration order. */
const REASON_RANK: Record<AttentionReason, number> = {
  uncertain_outcome: 0,
  crash_recovered: 1,
  lease_expired: 2,
  provider_ambiguity: 3,
  cancel_unconfirmed: 4,
  deadline_exceeded: 5,
  stream_gap: 6,
  stale_observation: 7,
};

/**
 * Whether `value` holds a C0 control character or DEL.
 *
 * Written as a codepoint scan rather than a regex so the source file itself
 * stays free of literal control bytes.
 */
function hasControlCharacter(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20 || code === 0x7f) return true;
  }
  return false;
}

/** Which kind of uncertainty a reason represents. */
export function reasonDomain(reason: AttentionReason): UncertaintyDomain {
  switch (reason) {
    case "uncertain_outcome":
    case "provider_ambiguity":
      return "model_or_provider";
    case "crash_recovered":
    case "lease_expired":
    case "stream_gap":
      return "worker_or_lease";
    case "cancel_unconfirmed":
    case "deadline_exceeded":
    case "stale_observation":
      return "operator_decision";
  }
}

/** A short operator-facing label for one reason. */
export function reasonLabel(reason: AttentionReason): string {
  switch (reason) {
    case "uncertain_outcome":
      return "Attempt outcome was never recorded";
    case "crash_recovered":
      return "Host restarted while the run was live";
    case "lease_expired":
      return "Worker lease expired";
    case "provider_ambiguity":
      return "Provider and local state disagree";
    case "cancel_unconfirmed":
      return "Cancel was never confirmed";
    case "deadline_exceeded":
      return "Run passed its deadline";
    case "stream_gap":
      return "Unread events were evicted";
    case "stale_observation":
      return "No fresh corroborating evidence";
  }
}

function asRecord(value: unknown, field: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new ReconciliationError(`GrokPtah ${field} must be an object`);
  }
  return value as Record<string, unknown>;
}

function asString(value: unknown, field: string): string {
  if (typeof value !== "string" || !value.trim()) {
    throw new ReconciliationError(`GrokPtah ${field} must be a non-empty string`);
  }
  return value;
}

function asSeq(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new ReconciliationError(`GrokPtah ${field} must be a non-negative safe integer`);
  }
  return value;
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).length;
}

/**
 * Parse an authority projection, refusing anything this build cannot read.
 *
 * A projection is untrusted input: it crosses a product boundary and drives
 * what an operator is told about a run they may be about to close out. An
 * unknown contract version is refused rather than best-effort rendered, so a
 * newer authority cannot silently show an operator a partial truth.
 */
export function parseRunAttention(value: unknown): RunAttention {
  const record = asRecord(value, "attention");
  const contract = asString(record.contract, "attention.contract");
  if (contract !== RECONCILIATION_CONTRACT) {
    throw new ReconciliationError(
      `GrokPtah reconciliation contract ${contract} is not supported by this build`,
    );
  }

  const state = asString(record.state, "attention.state");
  if (!RUN_STATES.has(state as DurableRunState)) {
    throw new ReconciliationError(`GrokPtah attention.state ${state} is not recognized`);
  }

  const confidence = asString(record.confidence, "attention.confidence");
  if (!CONFIDENCES.has(confidence as RunConfidence)) {
    throw new ReconciliationError(
      `GrokPtah attention.confidence ${confidence} is not recognized`,
    );
  }

  if (typeof record.needsAttention !== "boolean") {
    throw new ReconciliationError("GrokPtah attention.needsAttention must be a boolean");
  }

  if (!Array.isArray(record.reasons)) {
    throw new ReconciliationError("GrokPtah attention.reasons must be an array");
  }
  const reasons = record.reasons.map((reason, index) => {
    const parsed = asString(reason, `attention.reasons[${index}]`);
    if (!REASONS.has(parsed as AttentionReason)) {
      throw new ReconciliationError(`GrokPtah attention reason ${parsed} is not recognized`);
    }
    return parsed as AttentionReason;
  });

  if (!Array.isArray(record.domains)) {
    throw new ReconciliationError("GrokPtah attention.domains must be an array");
  }
  const domains = record.domains.map((domain, index) => {
    const parsed = asString(domain, `attention.domains[${index}]`);
    if (!DOMAINS.has(parsed as UncertaintyDomain)) {
      throw new ReconciliationError(`GrokPtah attention domain ${parsed} is not recognized`);
    }
    return parsed as UncertaintyDomain;
  });

  let severity: AttentionSeverity | undefined;
  if (record.severity !== undefined && record.severity !== null) {
    const parsed = asString(record.severity, "attention.severity");
    if (!SEVERITIES.has(parsed as AttentionSeverity)) {
      throw new ReconciliationError(`GrokPtah attention severity ${parsed} is not recognized`);
    }
    severity = parsed as AttentionSeverity;
  }

  // needsAttention and reasons must agree, or an operator queue silently
  // drops a run that the authority flagged.
  if (record.needsAttention !== reasons.length > 0) {
    throw new ReconciliationError(
      "GrokPtah attention.needsAttention disagrees with attention.reasons",
    );
  }

  return {
    contract: RECONCILIATION_CONTRACT,
    runRef: asString(record.runRef, "attention.runRef"),
    state: state as DurableRunState,
    confidence: confidence as RunConfidence,
    needsAttention: record.needsAttention,
    reasons,
    severity,
    domains,
    observedSeq: asSeq(record.observedSeq, "attention.observedSeq"),
    revision: asSeq(record.revision, "attention.revision"),
  };
}

/** The most severe reason present, or `null` when nothing needs attention. */
export function leadReason(attention: RunAttention): AttentionReason | null {
  if (attention.reasons.length === 0) return null;
  return [...attention.reasons].sort((left, right) => REASON_RANK[left] - REASON_RANK[right])[0];
}

/**
 * Order an operator queue: most severe first, then oldest observation first.
 *
 * Ties break on `runRef` so two surfaces rendering the same set agree on row
 * order. The input array is not mutated.
 */
export function sortByUrgency(items: RunAttention[]): RunAttention[] {
  return [...items].sort((left, right) => {
    const leftRank = left.severity ? SEVERITY_RANK[left.severity] : Number.MAX_SAFE_INTEGER;
    const rightRank = right.severity ? SEVERITY_RANK[right.severity] : Number.MAX_SAFE_INTEGER;
    if (leftRank !== rightRank) return leftRank - rightRank;
    if (left.observedSeq !== right.observedSeq) return left.observedSeq - right.observedSeq;
    return left.runRef < right.runRef ? -1 : left.runRef > right.runRef ? 1 : 0;
  });
}

/**
 * A compact block a self-hosted coding-agent operator can read directly.
 *
 * The domain line is the point: it tells the operator whether to go look at
 * the provider, at our worker, or at their own decision queue.
 */
export function summarizeForOperator(attention: RunAttention): string {
  const lines = [
    `run ${attention.runRef} - ${attention.state} (${attention.confidence})`,
    `revision ${attention.revision} @ seq ${attention.observedSeq}`,
  ];
  if (!attention.needsAttention) {
    lines.push("no operator action required");
    return lines.join("\n");
  }
  lines.push(`severity: ${attention.severity ?? "unknown"}`);
  lines.push(`domains: ${attention.domains.join(", ")}`);
  for (const reason of attention.reasons) {
    lines.push(`- ${reasonLabel(reason)} [${reasonDomain(reason)}]`);
  }
  lines.push(
    `fence the next reconcile on revision ${attention.revision}; reconciliation records evidence and resolves state, and never resends or retries the attempt`,
  );
  return lines.join("\n");
}

function validateEvidence(evidence: EvidenceRecord[]): EvidenceRecord[] {
  if (evidence.length > MAX_EVIDENCE_PER_REQUEST) {
    throw new ReconciliationError(
      `GrokPtah evidence must hold at most ${MAX_EVIDENCE_PER_REQUEST} records`,
    );
  }
  return evidence.map((record, index) => {
    if (!EVIDENCE_KINDS.has(record.kind)) {
      throw new ReconciliationError(`GrokPtah evidence[${index}].kind is not recognized`);
    }
    const digest = asString(record.digest, `evidence[${index}].digest`);
    if (/\s/.test(digest)) {
      throw new ReconciliationError(
        `GrokPtah evidence[${index}].digest must not contain whitespace`,
      );
    }
    const summary = asString(record.summary, `evidence[${index}].summary`);
    if (utf8Length(summary) > MAX_EVIDENCE_SUMMARY_BYTES) {
      throw new ReconciliationError(
        `GrokPtah evidence[${index}].summary exceeds ${MAX_EVIDENCE_SUMMARY_BYTES} bytes`,
      );
    }
    if (hasControlCharacter(summary)) {
      throw new ReconciliationError(
        `GrokPtah evidence[${index}].summary must not contain control characters`,
      );
    }
    return { kind: record.kind, digest, summary };
  });
}

/**
 * Build the wire payload for one reconciliation intent.
 *
 * Mirrors the authority's own validation so a bad request fails at the
 * operator's keyboard rather than after a round trip. The authority still
 * re-validates: this is a convenience fence, not a security boundary.
 */
export function buildReconcileRequest(
  input: ReconcileRequestInput,
): Record<string, unknown> {
  if (!RECONCILE_ACTIONS.includes(input.action)) {
    throw new ReconciliationError(
      `GrokPtah reconcile action ${String(input.action)} is not in the closed action set`,
    );
  }
  const evidence = validateEvidence(input.evidence ?? []);
  const resolving = input.action.startsWith("resolve_");
  if (resolving && evidence.length === 0) {
    throw new ReconciliationError(
      "GrokPtah reconcile actions that resolve an outcome require at least one evidence record",
    );
  }
  const note = input.note ?? "";
  if (utf8Length(note) > MAX_NOTE_BYTES) {
    throw new ReconciliationError(`GrokPtah note exceeds ${MAX_NOTE_BYTES} bytes`);
  }
  if (hasControlCharacter(note)) {
    throw new ReconciliationError("GrokPtah note must not contain control characters");
  }
  const expectedRevision = asSeq(input.expectedRevision, "expectedRevision");

  return {
    request_id: asString(input.requestId, "requestId"),
    session_id: asString(input.scope.sessionId, "sessionId"),
    workspace: asString(input.scope.workspace, "workspace"),
    run_id: asString(input.scope.runId, "runId"),
    expected_revision: expectedRevision,
    action: input.action,
    evidence,
    note,
    operator_ref: asString(input.operator.operatorRef, "operatorRef"),
    authority_ref: asString(input.operator.authorityRef, "authorityRef"),
  };
}

/** Build the read-only history request for one run. */
export function buildHistoryRequest(
  scope: ReconciliationScope,
  after?: number,
  limit = MAX_HISTORY_PAGE,
): Record<string, unknown> {
  if (after !== undefined) asSeq(after, "after");
  if (!Number.isSafeInteger(limit) || limit <= 0) {
    throw new ReconciliationError("GrokPtah history limit must be a positive safe integer");
  }
  return {
    session_id: asString(scope.sessionId, "sessionId"),
    workspace: asString(scope.workspace, "workspace"),
    run_id: asString(scope.runId, "runId"),
    ...(after === undefined ? {} : { after }),
    // The authority clamps as well; sending an honest bound keeps the two
    // surfaces from disagreeing about how much history was returned.
    limit: Math.min(limit, MAX_HISTORY_PAGE),
  };
}
