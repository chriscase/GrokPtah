import { describe, expect, it } from "vitest";

import {
  buildHistoryRequest,
  buildReconcileRequest,
  leadReason,
  MAX_EVIDENCE_PER_REQUEST,
  MAX_HISTORY_PAGE,
  parseRunAttention,
  reasonDomain,
  RECONCILE_ACTIONS,
  RECONCILIATION_CONTRACT,
  ReconciliationError,
  sortByUrgency,
  summarizeForOperator,
  type EvidenceRecord,
  type ReconcileAction,
  type RunAttention,
} from "./operatorReconciliation";

/**
 * The exact document produced by the Rust suite's
 * `the_crash_cut_projection_matches_the_cross_language_golden_fixture`.
 * Both sides assert this literal so the two implementations of one contract
 * cannot drift apart without a test failing.
 */
const GOLDEN_CRASH_CUT = {
  contract: "grokptah.operator-reconciliation.v1",
  runRef: "op-run-e4",
  state: "running",
  confidence: "uncertain",
  needsAttention: true,
  reasons: [
    "uncertain_outcome",
    "crash_recovered",
    "lease_expired",
    "provider_ambiguity",
    "stale_observation",
  ],
  severity: "blocking",
  domains: ["model_or_provider", "worker_or_lease", "operator_decision"],
  observedSeq: 88,
  revision: 12,
};

const SCOPE = {
  sessionId: "session-7f1c",
  workspace: "approved-alias",
  runId: "run-4b21",
};

const OPERATOR = { operatorRef: "operator-1", authorityRef: "authority-a1" };

const EVIDENCE: EvidenceRecord[] = [
  {
    kind: "provider_projection",
    digest: "sha256:5f0c9a",
    summary: "provider console shows the attempt never started",
  },
];

function attention(overrides: Partial<RunAttention> = {}): RunAttention {
  return { ...parseRunAttention(GOLDEN_CRASH_CUT), ...overrides };
}

describe("parseRunAttention", () => {
  it("accepts the cross-language golden crash-cut projection", () => {
    const parsed = parseRunAttention(GOLDEN_CRASH_CUT);
    expect(parsed.contract).toBe(RECONCILIATION_CONTRACT);
    expect(parsed.runRef).toBe("op-run-e4");
    expect(parsed.state).toBe("running");
    expect(parsed.confidence).toBe("uncertain");
    expect(parsed.needsAttention).toBe(true);
    expect(parsed.severity).toBe("blocking");
    expect(parsed.reasons).toEqual(GOLDEN_CRASH_CUT.reasons);
    expect(parsed.domains).toEqual(GOLDEN_CRASH_CUT.domains);
    expect(parsed.observedSeq).toBe(88);
    expect(parsed.revision).toBe(12);
  });

  it("keeps every reason's domain agreeing with the projection it came in", () => {
    const parsed = parseRunAttention(GOLDEN_CRASH_CUT);
    for (const reason of parsed.reasons) {
      expect(parsed.domains).toContain(reasonDomain(reason));
    }
  });

  it("refuses a contract version this build cannot read", () => {
    expect(() =>
      parseRunAttention({ ...GOLDEN_CRASH_CUT, contract: "grokptah.operator-reconciliation.v2" }),
    ).toThrow(ReconciliationError);
  });

  it("refuses an unknown reason rather than dropping it", () => {
    expect(() =>
      parseRunAttention({ ...GOLDEN_CRASH_CUT, reasons: ["uncertain_outcome", "sunspots"] }),
    ).toThrow(/reason sunspots is not recognized/);
  });

  it("refuses a projection whose flag disagrees with its reasons", () => {
    expect(() =>
      parseRunAttention({ ...GOLDEN_CRASH_CUT, needsAttention: false }),
    ).toThrow(/disagrees with attention.reasons/);
  });

  it("accepts a clean run with no reasons and no severity", () => {
    const clean = parseRunAttention({
      ...GOLDEN_CRASH_CUT,
      confidence: "confirmed",
      needsAttention: false,
      reasons: [],
      severity: null,
      domains: [],
    });
    expect(clean.needsAttention).toBe(false);
    expect(clean.severity).toBeUndefined();
    expect(leadReason(clean)).toBeNull();
  });

  it("rejects malformed shapes instead of coercing them", () => {
    expect(() => parseRunAttention(null)).toThrow(ReconciliationError);
    expect(() => parseRunAttention([GOLDEN_CRASH_CUT])).toThrow(ReconciliationError);
    expect(() => parseRunAttention({ ...GOLDEN_CRASH_CUT, observedSeq: -1 })).toThrow(
      ReconciliationError,
    );
    expect(() => parseRunAttention({ ...GOLDEN_CRASH_CUT, revision: 1.5 })).toThrow(
      ReconciliationError,
    );
  });
});

describe("operator queue ordering", () => {
  it("ranks blocking above degraded above advisory, then oldest first", () => {
    const items: RunAttention[] = [
      attention({ runRef: "run-advisory", severity: "advisory", observedSeq: 5 }),
      attention({ runRef: "run-blocking-late", severity: "blocking", observedSeq: 9 }),
      attention({ runRef: "run-degraded", severity: "degraded", observedSeq: 1 }),
      attention({ runRef: "run-blocking-early", severity: "blocking", observedSeq: 2 }),
    ];
    expect(sortByUrgency(items).map((item) => item.runRef)).toEqual([
      "run-blocking-early",
      "run-blocking-late",
      "run-degraded",
      "run-advisory",
    ]);
  });

  it("is deterministic on ties and does not mutate its input", () => {
    const items: RunAttention[] = [
      attention({ runRef: "run-b", severity: "blocking", observedSeq: 3 }),
      attention({ runRef: "run-a", severity: "blocking", observedSeq: 3 }),
    ];
    const snapshot = items.map((item) => item.runRef);
    expect(sortByUrgency(items).map((item) => item.runRef)).toEqual(["run-a", "run-b"]);
    expect(items.map((item) => item.runRef)).toEqual(snapshot);
  });

  it("sorts runs with no severity last", () => {
    const items: RunAttention[] = [
      attention({ runRef: "run-clean", severity: undefined, observedSeq: 1 }),
      attention({ runRef: "run-advisory", severity: "advisory", observedSeq: 99 }),
    ];
    expect(sortByUrgency(items).map((item) => item.runRef)).toEqual([
      "run-advisory",
      "run-clean",
    ]);
  });
});

describe("summarizeForOperator", () => {
  it("names the lead reason, every domain, and the revision to fence on", () => {
    const summary = summarizeForOperator(parseRunAttention(GOLDEN_CRASH_CUT));
    expect(summary).toContain("op-run-e4");
    expect(summary).toContain("running (uncertain)");
    expect(summary).toContain("severity: blocking");
    expect(summary).toContain("model_or_provider, worker_or_lease, operator_decision");
    expect(summary).toContain("Attempt outcome was never recorded [model_or_provider]");
    expect(summary).toContain("Worker lease expired [worker_or_lease]");
    expect(summary).toContain("fence the next reconcile on revision 12");
    expect(summary).toContain("never resends or retries the attempt");
  });

  it("says so plainly when nothing needs an operator", () => {
    const clean = attention({ needsAttention: false, reasons: [], domains: [], severity: undefined });
    expect(summarizeForOperator(clean)).toContain("no operator action required");
  });
});

describe("buildReconcileRequest", () => {
  it("emits the snake_case wire payload the authority parses", () => {
    const payload = buildReconcileRequest({
      requestId: "req-1",
      scope: SCOPE,
      expectedRevision: 12,
      action: "resolve_failed",
      evidence: EVIDENCE,
      note: "closing out after the worker crash",
      operator: OPERATOR,
    });
    expect(payload).toEqual({
      request_id: "req-1",
      session_id: "session-7f1c",
      workspace: "approved-alias",
      run_id: "run-4b21",
      expected_revision: 12,
      action: "resolve_failed",
      evidence: EVIDENCE,
      note: "closing out after the worker crash",
      operator_ref: "operator-1",
      authority_ref: "authority-a1",
    });
  });

  it("exposes a closed action set with no resend, retry, or resume member", () => {
    expect([...RECONCILE_ACTIONS]).toEqual([
      "record_evidence",
      "acknowledge",
      "resolve_completed",
      "resolve_failed",
      "resolve_cancelled",
    ]);
    for (const forbidden of ["retry", "resend", "resume", "rerun", "submit_task"]) {
      expect(RECONCILE_ACTIONS).not.toContain(forbidden);
      expect(() =>
        buildReconcileRequest({
          requestId: "req-1",
          scope: SCOPE,
          expectedRevision: 12,
          action: forbidden as ReconcileAction,
          evidence: EVIDENCE,
          operator: OPERATOR,
        }),
      ).toThrow(/not in the closed action set/);
    }
  });

  it("requires evidence before an outcome may be asserted", () => {
    for (const action of ["resolve_completed", "resolve_failed", "resolve_cancelled"] as const) {
      expect(() =>
        buildReconcileRequest({
          requestId: "req-1",
          scope: SCOPE,
          expectedRevision: 12,
          action,
          operator: OPERATOR,
        }),
      ).toThrow(/require at least one evidence record/);
    }
    // Acknowledging asserts nothing, so it needs nothing.
    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "acknowledge",
        operator: OPERATOR,
      }),
    ).not.toThrow();
  });

  it("bounds evidence count, summary size, and digest shape", () => {
    const many = Array.from({ length: MAX_EVIDENCE_PER_REQUEST + 1 }, (_unused, index) => ({
      kind: "operator_statement" as const,
      digest: `sha256:${index}`,
      summary: "bounded",
    }));
    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "record_evidence",
        evidence: many,
        operator: OPERATOR,
      }),
    ).toThrow(/at most 16 records/);

    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "record_evidence",
        evidence: [{ kind: "host_journal", digest: "sha256:aa", summary: "x".repeat(513) }],
        operator: OPERATOR,
      }),
    ).toThrow(/exceeds 512 bytes/);

    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "record_evidence",
        evidence: [{ kind: "host_journal", digest: "sha256 aa", summary: "ok" }],
        operator: OPERATOR,
      }),
    ).toThrow(/must not contain whitespace/);
  });

  it("bounds the note and rejects smuggled control characters", () => {
    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "record_evidence",
        note: "x".repeat(2049),
        operator: OPERATOR,
      }),
    ).toThrow(/exceeds 2048 bytes/);

    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "record_evidence",
        note: "first line\nsecond line",
        operator: OPERATOR,
      }),
    ).toThrow(/must not contain control characters/);
  });

  it("requires a revision fence and a complete identity", () => {
    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: -1,
        action: "acknowledge",
        operator: OPERATOR,
      }),
    ).toThrow(/expectedRevision/);

    expect(() =>
      buildReconcileRequest({
        requestId: " ",
        scope: SCOPE,
        expectedRevision: 12,
        action: "acknowledge",
        operator: OPERATOR,
      }),
    ).toThrow(/requestId/);

    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: { ...SCOPE, runId: "" },
        expectedRevision: 12,
        action: "acknowledge",
        operator: OPERATOR,
      }),
    ).toThrow(/runId/);

    expect(() =>
      buildReconcileRequest({
        requestId: "req-1",
        scope: SCOPE,
        expectedRevision: 12,
        action: "acknowledge",
        operator: { ...OPERATOR, authorityRef: "" },
      }),
    ).toThrow(/authorityRef/);
  });
});

describe("buildHistoryRequest", () => {
  it("omits an absent cursor and clamps an oversized page", () => {
    expect(buildHistoryRequest(SCOPE)).toEqual({
      session_id: "session-7f1c",
      workspace: "approved-alias",
      run_id: "run-4b21",
      limit: MAX_HISTORY_PAGE,
    });
    expect(buildHistoryRequest(SCOPE, 40, 5_000).limit).toBe(MAX_HISTORY_PAGE);
    expect(buildHistoryRequest(SCOPE, 40, 8)).toMatchObject({ after: 40, limit: 8 });
  });

  it("rejects a cursor or page size that is not a sane integer", () => {
    expect(() => buildHistoryRequest(SCOPE, -1)).toThrow(ReconciliationError);
    expect(() => buildHistoryRequest(SCOPE, 1.5)).toThrow(ReconciliationError);
    expect(() => buildHistoryRequest(SCOPE, 0, 0)).toThrow(ReconciliationError);
  });
});
