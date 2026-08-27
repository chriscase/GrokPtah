import { describe, expect, it } from "vitest";

import {
  ADAPTIVE_COMPUTER_USE_CONTRACT,
  ADAPTIVE_DECISION_REQUEST_SCHEMA,
  ADAPTIVE_MAX_ANSWER_BYTES,
  ADAPTIVE_MAX_CANDIDATES,
  ADAPTIVE_MAX_ELEMENTS,
  ADAPTIVE_MAX_REQUEST_ELEMENTS,
  ADAPTIVE_STATIONARY_LIMIT,
  ADAPTIVE_UNCERTAINTY_LIMIT,
  adaptiveAdoptModelDecision,
  adaptiveAuthorizePlan,
  adaptiveCommitPlan,
  adaptiveDecideStep,
  adaptiveDefaultBudget,
  adaptiveIngestObservation,
  adaptiveRecordVerification,
  adaptiveStepProjection,
  adaptiveVerifyPlan,
  buildAdaptiveDecisionRequest,
  createAdaptiveController,
  negotiateAdaptiveCapabilities,
  parseAdaptiveCandidate,
  parseAdaptiveDecisionAnswer,
  parseAdaptiveElement,
  parseAdaptiveObservation,
} from "./adaptiveComputerUse";
import type {
  AdaptiveActionPlan,
  AdaptiveCandidateAction,
  AdaptiveControllerState,
  AdaptiveDecisionRequest,
  AdaptiveObservation,
} from "./adaptiveComputerUse";
import {
  ADAPTIVE_FORBIDDEN_MARKERS,
  ADAPTIVE_INVALID_REPLIES,
  ADAPTIVE_REJECTION_FIXTURES,
  FIXTURE_AMBIGUOUS_CANDIDATES,
  FIXTURE_RUN_ID,
  FIXTURE_SINGLE_CANDIDATE,
  FRAME_A,
  FRAME_B,
  VALUE_DIGEST_FILLED,
  fixtureObservation,
  fixtureRunProjection,
} from "./adaptiveComputerUse.fixtures";
import type { CapabilitySet } from "./capabilities";

/* -------------------------------------------------------------------------- */
/* Harness                                                                     */
/* -------------------------------------------------------------------------- */

type Profile = "economy" | "balanced" | "high_assurance";

function controller(profile: Profile = "balanced"): AdaptiveControllerState {
  const state = createAdaptiveController({ runId: FIXTURE_RUN_ID, profile });
  if (!state) throw new Error("fixture controller could not be created");
  return state;
}

/** Ingest an observation and assert it was accepted, returning the new state. */
function ingest(
  state: AdaptiveControllerState,
  observation: AdaptiveObservation,
): AdaptiveControllerState {
  const result = adaptiveIngestObservation(state, observation, fixtureRunProjection());
  if (!result.ok) throw new Error(`fixture observation rejected: ${result.reason}`);
  return result.state;
}

function reply(candidateId: string, confidence: number, abstain = false): string {
  return JSON.stringify({
    candidateId,
    confidence,
    rationaleCode: "matches_goal_semantics",
    abstain,
  });
}

function requestFor(
  state: AdaptiveControllerState,
  candidates: readonly AdaptiveCandidateAction[] = FIXTURE_AMBIGUOUS_CANDIDATES,
): AdaptiveDecisionRequest {
  const decision = adaptiveDecideStep(state, candidates);
  if (decision.kind !== "consult" && decision.kind !== "escalate") {
    throw new Error(`expected a model request, got ${decision.kind}`);
  }
  return decision.request;
}

/* -------------------------------------------------------------------------- */
/* Bounded schemas                                                             */
/* -------------------------------------------------------------------------- */

describe("bounded schemas", () => {
  it("accepts the healthy fixture observation and candidates", () => {
    expect(parseAdaptiveObservation(fixtureObservation())).not.toBeNull();
    for (const candidate of FIXTURE_AMBIGUOUS_CANDIDATES) {
      expect(parseAdaptiveCandidate(candidate)).not.toBeNull();
    }
  });

  it.each(ADAPTIVE_REJECTION_FIXTURES)("rejects $name", ({ parser, value }) => {
    const parsed =
      parser === "element"
        ? parseAdaptiveElement(value)
        : parser === "observation"
          ? parseAdaptiveObservation(value)
          : parseAdaptiveCandidate(value);
    expect(parsed).toBeNull();
  });

  it("rejects an unbounded element list", () => {
    const elements = Array.from({ length: ADAPTIVE_MAX_ELEMENTS + 1 }, (_entry, index) => ({
      elementId: `element-${index}`,
      role: "button",
      enabled: true,
      focused: false,
      sensitivity: "normal" as const,
      actionClasses: ["invoke"],
    }));
    expect(parseAdaptiveObservation(fixtureObservation({ elements }))).toBeNull();
  });

  it("rejects an unknown key rather than ignoring it", () => {
    expect(
      parseAdaptiveObservation({ ...fixtureObservation(), operatorNote: "please hurry" }),
    ).toBeNull();
  });

  it("rejects a frame digest that is not opaque hex", () => {
    expect(parseAdaptiveObservation(fixtureObservation({ frameDigest: "not-a-digest" }))).toBeNull();
  });

  it("closes the generic execute escape by enumerating action kinds", () => {
    const kinds = FIXTURE_AMBIGUOUS_CANDIDATES.map((candidate) => candidate.kind);
    expect(kinds).not.toContain("execute");
    expect(
      parseAdaptiveCandidate({
        candidateId: "cand-arbitrary",
        kind: "run_command",
        elementId: "button-save",
        actionClass: "invoke",
        mutating: true,
        authorized: true,
        expectation: { kind: "frame_changed" },
      }),
    ).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Deterministic and small-model happy paths                                   */
/* -------------------------------------------------------------------------- */

describe("cheap-path decisions", () => {
  it("acts deterministically with no model when exactly one action is authorized", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);

    expect(decision.kind).toBe("act");
    if (decision.kind !== "act") return;
    expect(decision.modelClass).toBe("none");
    expect(decision.plan.decidedBy).toBe("none");
    expect(decision.plan.confidence).toBe(1);
    expect(decision.plan.rationaleCode).toBe("only_authorized_action");

    const committed = adaptiveCommitPlan(state, decision.plan);
    expect(committed?.usage.smallModelCalls).toBe(0);
    expect(committed?.usage.largeModelCalls).toBe(0);
  });

  it("prefers the deterministic path even under high assurance", () => {
    const state = ingest(controller("high_assurance"), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    expect(decision.kind === "act" && decision.modelClass).toBe("none");
  });

  it("completes a small-model happy path without ever calling a large model", () => {
    let state = ingest(controller(), fixtureObservation());

    const request = requestFor(state);
    expect(request.modelClass).toBe("small");
    expect(request.schema).toBe(ADAPTIVE_DECISION_REQUEST_SCHEMA);
    expect(request.escalationReason).toBeNull();

    const adopted = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-title", 0.92),
      FIXTURE_AMBIGUOUS_CANDIDATES,
      { tokens: 180, latencyMs: 240 },
    );
    expect(adopted.decision.kind).toBe("act");
    if (adopted.decision.kind !== "act") return;
    expect(adopted.decision.modelClass).toBe("small");
    state = adopted.state;

    const committed = adaptiveCommitPlan(state, adopted.decision.plan, { latencyMs: 60 });
    expect(committed).not.toBeNull();
    if (!committed) return;
    state = committed;

    const after = fixtureObservation({
      observationId: "obs-2",
      revision: 2,
      frameDigest: FRAME_B,
      elements: fixtureObservation().elements.map((element) =>
        element.elementId === "field-title"
          ? { ...element, valueDigest: VALUE_DIGEST_FILLED }
          : element,
      ),
    });
    const verdict = adaptiveVerifyPlan(adopted.decision.plan, fixtureObservation(), after);
    expect(verdict?.status).toBe("verified");
    if (!verdict) return;

    const recorded = adaptiveRecordVerification(state, verdict);
    expect(recorded.decision).toBeNull();
    expect(recorded.state.usage.largeModelCalls).toBe(0);
    expect(recorded.state.escalations).toEqual([]);
    expect(recorded.state.halted).toBeNull();
  });

  it("bounds candidates and element context handed to a model", () => {
    const elements = Array.from({ length: ADAPTIVE_MAX_ELEMENTS }, (_entry, index) => ({
      elementId: `element-${index}`,
      role: "button",
      enabled: true,
      focused: false,
      sensitivity: "normal" as const,
      actionClasses: ["invoke"],
    }));
    const candidates: AdaptiveCandidateAction[] = Array.from(
      { length: ADAPTIVE_MAX_CANDIDATES + 8 },
      (_entry, index) => ({
        candidateId: `cand-${index}`,
        kind: "invoke",
        elementId: `element-${index % ADAPTIVE_MAX_ELEMENTS}`,
        actionClass: "invoke",
        mutating: true,
        authorized: true,
        expectation: { kind: "frame_changed" },
      }),
    );
    const state = ingest(controller(), fixtureObservation({ elements }));
    const request = buildAdaptiveDecisionRequest(state, candidates, "small");

    expect(request?.candidates).toHaveLength(ADAPTIVE_MAX_CANDIDATES);
    expect(request?.elements).toHaveLength(ADAPTIVE_MAX_REQUEST_ELEMENTS);
    expect(request?.elementsTruncated).toBe(true);
    expect(request?.grammar.json.properties.candidateId.enum).toHaveLength(
      ADAPTIVE_MAX_CANDIDATES,
    );
  });

  it("never shows an unauthorized candidate to a model", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    expect(request.candidates.map((candidate) => candidate.candidateId)).toEqual([
      "cand-save",
      "cand-title",
    ]);
    expect(JSON.stringify(request)).not.toContain("cand-publish");
  });

  it("withholds restricted labels and value references from the model context", () => {
    const observation = fixtureObservation({
      elements: [
        {
          elementId: "field-secret",
          role: "textField",
          label: "Recovery phrase",
          enabled: true,
          focused: true,
          sensitivity: "restricted",
          actionClasses: ["set_value"],
        },
      ],
    });
    const state = ingest(controller(), observation);
    const request = buildAdaptiveDecisionRequest(state, FIXTURE_AMBIGUOUS_CANDIDATES, "small");

    expect(request?.elements[0]?.labelRedacted).toBe(true);
    expect(request?.elements[0]?.label).toBeUndefined();
    const serialized = JSON.stringify(request);
    expect(serialized).not.toContain("Recovery phrase");
    expect(serialized).not.toContain("valueref-title-draft");
    expect(serialized).not.toContain(FRAME_A);
  });
});

/* -------------------------------------------------------------------------- */
/* Grammar-constrained output                                                  */
/* -------------------------------------------------------------------------- */

describe("grammar-constrained model output", () => {
  it("emits a grammar that enumerates exactly the offered candidates", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);

    expect(request.grammar.format).toBe("json-object");
    expect(request.grammar.gbnf).toContain('candidate ::= "\\"cand-save\\"" | "\\"cand-title\\""');
    expect(request.grammar.gbnf).not.toContain("cand-publish");
    expect(request.grammar.json.additionalProperties).toBe(false);
    // No production may emit free text; a prose field would defeat the point.
    expect(request.grammar.gbnf).not.toContain("char");
    expect(Object.keys(request.grammar.json.properties)).toEqual([
      "candidateId",
      "confidence",
      "rationaleCode",
      "abstain",
    ]);
  });

  it.each(ADAPTIVE_INVALID_REPLIES)("rejects $name", ({ reply: raw }) => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    expect(parseAdaptiveDecisionAnswer(raw, request)).toBeNull();
  });

  it("rejects a reply that exceeds the output ceiling", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    const padded = `{"candidateId":"cand-save","confidence":0.9,"rationaleCode":"${"x".repeat(
      ADAPTIVE_MAX_ANSWER_BYTES,
    )}","abstain":false}`;
    expect(padded.length).toBeGreaterThan(ADAPTIVE_MAX_ANSWER_BYTES);
    expect(parseAdaptiveDecisionAnswer(padded, request)).toBeNull();
  });

  it("carries no raw model prose into an adopted plan", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    const adopted = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-save", 0.95),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(adopted.decision.kind).toBe("act");
    if (adopted.decision.kind !== "act") return;
    // Every field on a plan is an id, a number, an enum, or a boolean.
    expect(adopted.decision.plan.rationaleCode).toBe("matches_goal_semantics");
    expect(Object.keys(adopted.decision.plan)).not.toContain("rationale");
    expect(Object.keys(adopted.decision.plan)).not.toContain("text");
  });

  it("charges a rejected answer to the budget rather than retrying for free", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    const adopted = adaptiveAdoptModelDecision(
      state,
      request,
      "I refuse to answer in JSON",
      FIXTURE_AMBIGUOUS_CANDIDATES,
      { tokens: 90, latencyMs: 120 },
    );
    expect(adopted.decision).toEqual({ kind: "abstain", reason: "model_output_rejected" });
    expect(adopted.state.usage.smallModelCalls).toBe(1);
    expect(adopted.state.usage.tokens).toBe(90);
    expect(adopted.state.uncertainStreak).toBe(1);
  });
});

/* -------------------------------------------------------------------------- */
/* Confidence and abstention                                                   */
/* -------------------------------------------------------------------------- */

describe("confidence and abstention", () => {
  it("abstains below the profile confidence floor instead of acting", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    expect(request.confidenceFloor).toBe(0.7);

    const adopted = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-save", 0.4),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(adopted.decision).toEqual({ kind: "abstain", reason: "low_confidence" });
    expect(adopted.state.uncertainStreak).toBe(1);
  });

  it("applies a stricter floor under high assurance than under economy", () => {
    const economy = requestFor(ingest(controller("economy"), fixtureObservation()));
    const assured = requestFor(ingest(controller("high_assurance"), fixtureObservation()));
    expect(economy.confidenceFloor).toBeLessThan(assured.confidenceFloor);

    const adopted = adaptiveAdoptModelDecision(
      ingest(controller("high_assurance"), fixtureObservation()),
      assured,
      reply("cand-save", 0.8),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(adopted.decision).toEqual({ kind: "abstain", reason: "low_confidence" });
  });

  it("honours an explicit model abstention", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    const adopted = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-save", 0.99, true),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(adopted.decision).toEqual({ kind: "abstain", reason: "model_abstained" });
  });

  it("escalates after repeated uncertainty rather than re-asking forever", () => {
    let state = ingest(controller(), fixtureObservation());
    for (let attempt = 0; attempt < ADAPTIVE_UNCERTAINTY_LIMIT; attempt += 1) {
      const request = requestFor(state);
      expect(request.modelClass).toBe("small");
      state = adaptiveAdoptModelDecision(
        state,
        request,
        reply("cand-save", 0.2),
        FIXTURE_AMBIGUOUS_CANDIDATES,
      ).state;
    }
    expect(state.uncertainStreak).toBe(ADAPTIVE_UNCERTAINTY_LIMIT);

    const decision = adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES);
    expect(decision.kind).toBe("escalate");
    if (decision.kind !== "escalate") return;
    expect(decision.reason).toBe("repeated_uncertainty");
    expect(decision.request.modelClass).toBe("large");
  });
});

/* -------------------------------------------------------------------------- */
/* Explicit escalation reasons                                                 */
/* -------------------------------------------------------------------------- */

describe("escalation reasons", () => {
  it("escalates a screenshot-only surface", () => {
    const observation = fixtureObservation({
      surface: "screenshot_only",
      axAvailable: false,
      domAvailable: false,
      elements: [],
    });
    const decision = adaptiveDecideStep(
      ingest(controller(), observation),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(decision.kind === "escalate" && decision.reason).toBe("screenshot_only_surface");
  });

  it("escalates when both AX and DOM semantics are missing", () => {
    const observation = fixtureObservation({
      surface: "mixed",
      axAvailable: true,
      domAvailable: false,
    });
    // The parser guarantees a semantic surface has backing, so the missing
    // case is reached by an in-memory state a host adapter could produce.
    const state = ingest(controller(), observation);
    const degraded: AdaptiveControllerState = {
      ...state,
      observation: { ...observation, axAvailable: false, domAvailable: false },
    };
    const decision = adaptiveDecideStep(degraded, FIXTURE_AMBIGUOUS_CANDIDATES);
    expect(decision.kind === "escalate" && decision.reason).toBe("missing_semantics");
  });

  it("escalates contradictory AX/DOM data", () => {
    const observation = fixtureObservation({
      domAvailable: true,
      contradictions: ["ax_dom_role_mismatch"],
    });
    const decision = adaptiveDecideStep(
      ingest(controller(), observation),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(decision.kind === "escalate" && decision.reason).toBe("contradictory_semantics");
    if (decision.kind !== "escalate") return;
    expect(decision.request.escalationReason).toBe("contradictory_semantics");
    expect(decision.request.contradictions).toEqual(["ax_dom_role_mismatch"]);
  });

  it("refuses to buy a large model under the economy profile", () => {
    const observation = fixtureObservation({
      surface: "screenshot_only",
      axAvailable: false,
      domAvailable: false,
      elements: [],
    });
    const decision = adaptiveDecideStep(
      ingest(controller("economy"), observation),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(decision).toEqual({ kind: "abstain", reason: "escalation_not_permitted" });
    expect(adaptiveDefaultBudget("economy").maxLargeModelCalls).toBe(0);
  });
});

/* -------------------------------------------------------------------------- */
/* Stationarity                                                                */
/* -------------------------------------------------------------------------- */

describe("no-op and stationarity detection", () => {
  it("does not loop when identical frames repeat", () => {
    let state = ingest(controller(), fixtureObservation());
    for (let revision = 2; revision <= 1 + ADAPTIVE_STATIONARY_LIMIT; revision += 1) {
      state = ingest(
        state,
        fixtureObservation({ observationId: `obs-${revision}`, revision, frameDigest: FRAME_A }),
      );
    }
    expect(state.frameRepeat).toBe(ADAPTIVE_STATIONARY_LIMIT);

    const decision = adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES);
    expect(decision.kind).toBe("escalate");
    if (decision.kind !== "escalate") return;
    expect(decision.reason).toBe("no_op_detected");
  });

  it("resets the repeat counter once the frame actually changes", () => {
    let state = ingest(controller(), fixtureObservation());
    state = ingest(state, fixtureObservation({ observationId: "obs-2", revision: 2 }));
    expect(state.frameRepeat).toBe(1);
    state = ingest(
      state,
      fixtureObservation({ observationId: "obs-3", revision: 3, frameDigest: FRAME_B }),
    );
    expect(state.frameRepeat).toBe(0);
  });

  it("reports a mutating action that changed nothing as stationary", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const after = fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_A });
    const verdict = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after);
    expect(verdict?.status).toBe("stationary");
    expect(verdict?.frameChanged).toBe(false);
  });
});

/* -------------------------------------------------------------------------- */
/* Staleness                                                                   */
/* -------------------------------------------------------------------------- */

describe("stale observations and plans", () => {
  it("rejects a replayed observation revision", () => {
    const state = ingest(controller(), fixtureObservation());
    const replayed = adaptiveIngestObservation(
      state,
      fixtureObservation({ observationId: "obs-replay" }),
      fixtureRunProjection(),
    );
    expect(replayed.ok).toBe(false);
    expect(replayed.ok === false && replayed.reason).toBe("stale_revision");
    expect(replayed.state.revision).toBe(1);
  });

  it("refuses to mutate with a plan bound to a superseded revision", () => {
    const before = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(before, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const advanced = ingest(
      before,
      fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_B }),
    );
    const authorization = adaptiveAuthorizePlan(advanced, decision.plan);
    expect(authorization).toEqual({ authorized: false, reason: "stale_revision" });
    expect(adaptiveCommitPlan(advanced, decision.plan)).toBeNull();
    expect(advanced.usage.steps).toBe(0);
  });

  it("refuses a plan from a different control epoch", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");
    const forged: AdaptiveActionPlan = { ...decision.plan, controlEpoch: 8 };
    expect(adaptiveAuthorizePlan(state, forged)).toEqual({
      authorized: false,
      reason: "epoch_changed",
    });
    expect(adaptiveCommitPlan(state, forged)).toBeNull();
  });

  it("refuses a plan from a different run", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");
    const forged: AdaptiveActionPlan = { ...decision.plan, runId: "run-other" };
    expect(adaptiveAuthorizePlan(state, forged)).toEqual({
      authorized: false,
      reason: "run_mismatch",
    });
  });

  it("rejects a model answer that arrived after the observation moved on", () => {
    const state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    const advanced = ingest(
      state,
      fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_B }),
    );
    const adopted = adaptiveAdoptModelDecision(
      advanced,
      request,
      reply("cand-save", 0.99),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    );
    expect(adopted.decision).toEqual({ kind: "abstain", reason: "model_output_rejected" });
  });

  it("refuses to verify against an observation that is not newer", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");
    expect(adaptiveVerifyPlan(decision.plan, fixtureObservation(), fixtureObservation())).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Authority                                                                   */
/* -------------------------------------------------------------------------- */

describe("authority is borrowed from the durable projection", () => {
  it.each([
    ["operator takeover", { controlDisposition: "operator_takeover" as const }],
    ["stopped run", { controlDisposition: "stopped" as const }],
    ["terminal run", { terminal: true }],
    ["revoked grant", { grant: { ...fixtureRunProjection().grant!, revoked: true } }],
    ["expired grant", { grant: { ...fixtureRunProjection().grant!, expired: true } }],
    ["exhausted grant", { grant: { ...fixtureRunProjection().grant!, usesRemaining: 0 } }],
    ["absent grant", { grant: null }],
  ])("halts on %s", (_name, overrides) => {
    const result = adaptiveIngestObservation(
      controller(),
      fixtureObservation(),
      fixtureRunProjection(overrides),
    );
    expect(result.ok).toBe(false);
    expect(result.ok === false && result.reason).toBe("authority_not_agent_owned");
    expect(result.state.halted).toBe("authority_lost");
    expect(adaptiveDecideStep(result.state, FIXTURE_SINGLE_CANDIDATE)).toEqual({
      kind: "halt",
      reason: "authority_lost",
    });
  });

  it("rejects an observation whose epoch disagrees with the projection", () => {
    const result = adaptiveIngestObservation(
      controller(),
      fixtureObservation({ controlEpoch: 99 }),
      fixtureRunProjection(),
    );
    expect(result.ok === false && result.reason).toBe("epoch_changed");
  });

  it("negotiates capabilities without asking for more than the host granted", () => {
    const gated: CapabilitySet = {
      contract: "grokptah.capabilities.v1",
      capabilities: [
        {
          id: "computer.observe",
          tier: "computer_observe",
          mutating: false,
          human_gate: false,
          availability: "available",
          description: "Observe the authorized target",
        },
        {
          id: "computer.control",
          tier: "computer_control",
          mutating: true,
          human_gate: true,
          availability: "gated",
          description: "Act on the authorized target",
        },
      ],
    };
    expect(negotiateAdaptiveCapabilities(gated)).toEqual({
      observe: true,
      control: false,
      ready: false,
      missing: ["computer.control"],
    });
    expect(negotiateAdaptiveCapabilities(gated, { gateSatisfied: true }).ready).toBe(true);
    expect(negotiateAdaptiveCapabilities(null).missing).toEqual([
      "computer.observe",
      "computer.control",
    ]);
  });
});

/* -------------------------------------------------------------------------- */
/* Verification                                                                */
/* -------------------------------------------------------------------------- */

describe("verification", () => {
  it("escalates once on a failed verification instead of retrying blindly", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const after = fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_A });
    const verdict = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after);
    if (!verdict) throw new Error("expected a verifier result");

    const first = adaptiveRecordVerification(state, verdict);
    expect(first.decision).toBeNull();
    expect(first.state.pendingEscalation).toBe("no_op_detected");
    expect(first.state.failedVerifications).toEqual([decision.plan.planId]);

    // The next decision escalates rather than replaying the same move.
    const next = adaptiveDecideStep(first.state, FIXTURE_AMBIGUOUS_CANDIDATES);
    expect(next.kind).toBe("escalate");
    if (next.kind !== "escalate") return;
    expect(next.reason).toBe("no_op_detected");

    // A second failure of the same plan halts instead of escalating again.
    const second = adaptiveRecordVerification(first.state, verdict);
    expect(second.decision).toEqual({ kind: "halt", reason: "verification_exhausted" });
    expect(second.state.halted).toBe("verification_exhausted");
    expect(second.state.escalations).toHaveLength(1);
  });

  it("reports a wrong-direction change as contradicted, not merely unverified", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, [FIXTURE_AMBIGUOUS_CANDIDATES[1]]);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const after = fixtureObservation({
      observationId: "obs-2",
      revision: 2,
      frameDigest: FRAME_B,
    });
    expect(adaptiveVerifyPlan(decision.plan, fixtureObservation(), after)?.status).toBe(
      "contradicted",
    );
  });

  it("requires independent verification under high assurance", () => {
    const state = ingest(controller("high_assurance"), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const after = fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_B });

    const selfChecked = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after);
    expect(selfChecked?.status).toBe("verified");
    expect(selfChecked?.independent).toBe(false);
    if (!selfChecked) return;
    const withoutIndependent = adaptiveRecordVerification(state, selfChecked);
    expect(withoutIndependent.state.pendingEscalation).toBe("independent_verification_required");

    const independent = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after, {
      independent: true,
    });
    if (!independent) return;
    const withIndependent = adaptiveRecordVerification(state, independent);
    expect(withIndependent.state.pendingEscalation).toBeNull();
    expect(withIndependent.state.escalations).toEqual([]);
  });

  it("accepts a self-verified step under balanced", () => {
    const state = ingest(controller("balanced"), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");
    const after = fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_B });
    const verdict = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after);
    if (!verdict) return;
    expect(adaptiveRecordVerification(state, verdict).state.pendingEscalation).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Budgets                                                                     */
/* -------------------------------------------------------------------------- */

describe("budgets", () => {
  it("caps steps", () => {
    const base = createAdaptiveController({
      runId: FIXTURE_RUN_ID,
      budget: { maxSteps: 2 },
    });
    if (!base) throw new Error("controller not created");
    let state = ingest(base, fixtureObservation());
    expect(state.budget.maxSteps).toBe(2);

    for (let step = 0; step < 2; step += 1) {
      const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
      if (decision.kind !== "act") throw new Error(`expected act, got ${decision.kind}`);
      const committed = adaptiveCommitPlan(state, decision.plan);
      if (!committed) throw new Error("commit refused");
      state = committed;
    }
    expect(adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE)).toEqual({
      kind: "halt",
      reason: "budget_steps_exhausted",
    });
  });

  it("caps tokens", () => {
    const base = createAdaptiveController({ runId: FIXTURE_RUN_ID, budget: { maxTokens: 100 } });
    if (!base) throw new Error("controller not created");
    let state = ingest(base, fixtureObservation());
    const request = requestFor(state);
    state = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-save", 0.95),
      FIXTURE_AMBIGUOUS_CANDIDATES,
      { tokens: 100 },
    ).state;
    expect(adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES)).toEqual({
      kind: "halt",
      reason: "budget_tokens_exhausted",
    });
  });

  it("caps latency", () => {
    const base = createAdaptiveController({ runId: FIXTURE_RUN_ID, budget: { maxLatencyMs: 500 } });
    if (!base) throw new Error("controller not created");
    let state = ingest(base, fixtureObservation());
    const request = requestFor(state);
    state = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-save", 0.95),
      FIXTURE_AMBIGUOUS_CANDIDATES,
      { latencyMs: 500 },
    ).state;
    expect(adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES)).toEqual({
      kind: "halt",
      reason: "budget_latency_exhausted",
    });
  });

  it("caps small-model calls", () => {
    const base = createAdaptiveController({
      runId: FIXTURE_RUN_ID,
      budget: { maxSmallModelCalls: 1 },
    });
    if (!base) throw new Error("controller not created");
    let state = ingest(base, fixtureObservation());
    const request = requestFor(state);
    state = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-save", 0.2),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    ).state;
    expect(adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES)).toEqual({
      kind: "halt",
      reason: "budget_small_model_exhausted",
    });
  });

  it("caps large-model escalations", () => {
    const base = createAdaptiveController({
      runId: FIXTURE_RUN_ID,
      budget: { maxLargeModelCalls: 1 },
    });
    if (!base) throw new Error("controller not created");
    const observation = fixtureObservation({
      surface: "screenshot_only",
      axAvailable: false,
      domAvailable: false,
      elements: [],
    });
    let state = ingest(base, observation);

    const escalation = adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES);
    if (escalation.kind !== "escalate") throw new Error("expected an escalation");
    state = adaptiveAdoptModelDecision(
      state,
      escalation.request,
      reply("cand-save", 0.2),
      FIXTURE_AMBIGUOUS_CANDIDATES,
    ).state;
    expect(state.usage.largeModelCalls).toBe(1);

    expect(adaptiveDecideStep(state, FIXTURE_AMBIGUOUS_CANDIDATES)).toEqual({
      kind: "halt",
      reason: "budget_large_model_exhausted",
    });
  });

  it("clamps a caller that tries to widen a profile ceiling", () => {
    const ceiling = adaptiveDefaultBudget("economy");
    const state = createAdaptiveController({
      runId: FIXTURE_RUN_ID,
      profile: "economy",
      budget: {
        maxSteps: 10_000,
        maxLargeModelCalls: 50,
        maxTokens: 10_000_000,
        maxLatencyMs: Number.MAX_SAFE_INTEGER,
      },
    });
    expect(state?.budget).toEqual(ceiling);
  });

  it("lets a caller tighten a ceiling", () => {
    const state = createAdaptiveController({
      runId: FIXTURE_RUN_ID,
      profile: "balanced",
      budget: { maxSteps: 3 },
    });
    expect(state?.budget.maxSteps).toBe(3);
    expect(state?.budget.maxTokens).toBe(adaptiveDefaultBudget("balanced").maxTokens);
  });
});

/* -------------------------------------------------------------------------- */
/* Public projection hygiene                                                   */
/* -------------------------------------------------------------------------- */

describe("public projection", () => {
  it("carries no secrets, host paths, or frame bytes", () => {
    let state = ingest(controller(), fixtureObservation());
    const request = requestFor(state);
    const adopted = adaptiveAdoptModelDecision(
      state,
      request,
      reply("cand-title", 0.95),
      FIXTURE_AMBIGUOUS_CANDIDATES,
      { tokens: 200, latencyMs: 300 },
    );
    state = adopted.state;
    if (adopted.decision.kind !== "act") throw new Error("expected an act");
    const committed = adaptiveCommitPlan(state, adopted.decision.plan);
    if (!committed) throw new Error("commit refused");

    const projection = adaptiveStepProjection(committed);
    const serialized = JSON.stringify(projection);

    for (const marker of ADAPTIVE_FORBIDDEN_MARKERS) {
      expect(serialized).not.toContain(marker);
    }
    // Frame digests, element labels, and value digests all stay behind.
    expect(serialized).not.toContain(FRAME_A);
    expect(serialized).not.toContain(FRAME_B);
    expect(serialized).not.toContain("Document title");
    expect(serialized).not.toContain(VALUE_DIGEST_FILLED);
    expect(serialized).not.toContain("field-title");
  });

  it("reports budget arithmetic a consumer can act on", () => {
    const state = ingest(controller(), fixtureObservation());
    const projection = adaptiveStepProjection(state);
    expect(projection.contract).toBe(ADAPTIVE_COMPUTER_USE_CONTRACT);
    expect(projection.surface).toBe("semantic");
    expect(projection.elementCount).toBe(3);
    expect(projection.budgetRemaining.steps).toBe(adaptiveDefaultBudget("balanced").maxSteps);
    expect(projection.halted).toBeNull();
  });

  it("surfaces escalation history as codes, never as narrative text", () => {
    const state = ingest(controller(), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const stalled = fixtureObservation({
      observationId: "obs-2",
      revision: 2,
      frameDigest: FRAME_A,
    });
    const verdict = adaptiveVerifyPlan(decision.plan, fixtureObservation(), stalled);
    if (!verdict) throw new Error("expected a verifier result");

    const projection = adaptiveStepProjection(adaptiveRecordVerification(state, verdict).state);
    expect(projection.pendingEscalation).toBe("no_op_detected");
    expect(projection.escalationReasons).toEqual(["no_op_detected"]);
    expect(projection.lastVerificationStatus).toBe("stationary");
    // The model-facing instruction text stays on the request, not the
    // projection, so no prompt or rationale prose can reach a consumer.
    expect(Object.keys(projection)).not.toContain("instruction");
    expect(JSON.stringify(projection)).not.toContain("candidate");
  });

  it("holds a high-assurance run until an independent verifier reports", () => {
    const state = ingest(controller("high_assurance"), fixtureObservation());
    const decision = adaptiveDecideStep(state, FIXTURE_SINGLE_CANDIDATE);
    if (decision.kind !== "act") throw new Error("expected a deterministic act");

    const after = fixtureObservation({ observationId: "obs-2", revision: 2, frameDigest: FRAME_B });
    const selfChecked = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after);
    if (!selfChecked) throw new Error("expected a verifier result");

    const held = adaptiveRecordVerification(state, selfChecked).state;
    // The hold wants a second verifier, not a large model re-picking an action.
    expect(adaptiveDecideStep(held, FIXTURE_AMBIGUOUS_CANDIDATES)).toEqual({
      kind: "abstain",
      reason: "independent_verification_required",
    });
    expect(held.usage.largeModelCalls).toBe(0);

    const independent = adaptiveVerifyPlan(decision.plan, fixtureObservation(), after, {
      independent: true,
    });
    if (!independent) throw new Error("expected an independent verifier result");
    const released = adaptiveRecordVerification(held, independent).state;
    expect(released.pendingEscalation).toBeNull();
    expect(adaptiveDecideStep(released, FIXTURE_SINGLE_CANDIDATE).kind).toBe("act");
  });
});
