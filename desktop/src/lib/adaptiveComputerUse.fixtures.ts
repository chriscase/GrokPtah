/**
 * Deterministic fixtures for the adaptive Computer Use controller.
 *
 * Everything here is a stable product contract, not sampled model output. No
 * fixture contains a screenshot, a host path, a credential, a clipboard
 * payload, or an absolute URL; the adversarial fixtures that *do* contain such
 * markers exist precisely so the parsers can be shown rejecting them.
 */

import { ADAPTIVE_COMPUTER_USE_CONTRACT } from "./adaptiveComputerUse";
import type {
  AdaptiveCandidateAction,
  AdaptiveObservation,
} from "./adaptiveComputerUse";
import type { ComputerRunProjection } from "./protocol";

export const FIXTURE_RUN_ID = "run-adaptive-1";

/** Opaque frame digests. Two distinct frames plus one deliberate repeat. */
export const FRAME_A = "a1b2c3d4e5f60718";
export const FRAME_B = "b2c3d4e5f6071829";
export const VALUE_DIGEST_EMPTY = "0000000000000000";
export const VALUE_DIGEST_FILLED = "9f8e7d6c5b4a3928";

/**
 * An authoritative projection describing a live, agent-owned run.
 *
 * Overrides let a test flip exactly one authority fact — disposition, grant
 * revocation, terminality — without restating the whole record.
 */
export function fixtureRunProjection(
  overrides: Partial<ComputerRunProjection> = {},
): ComputerRunProjection {
  return {
    runId: FIXTURE_RUN_ID,
    ownerSessionId: "session-adaptive-1",
    target: {
      appId: "com.example.editor",
      windowId: "window-1",
      generation: 3,
      displayName: "Example Editor",
      sensitivity: "normal",
    },
    state: "running",
    controlDisposition: "agent_owned",
    controlEpoch: 7,
    version: 12,
    agentActive: true,
    terminal: false,
    createdAt: "2026-08-27T00:00:00Z",
    updatedAt: "2026-08-27T00:00:10Z",
    progress: {
      actionCount: 0,
      maxActions: 40,
      evidenceBytes: 0,
      maxEvidenceBytes: 1_048_576,
      elapsedMillis: 0,
      maxDurationSecs: 600,
      durationExceeded: false,
    },
    grant: {
      grantId: "grant-1",
      actionClasses: ["invoke", "set_value", "select", "scroll"],
      issuedBy: "operator",
      issuedAt: "2026-08-27T00:00:00Z",
      expiresAt: "2026-08-27T01:00:00Z",
      usesRemaining: 25,
      revoked: false,
      expired: false,
    },
    ...overrides,
  };
}

/** A healthy, AX-backed semantic observation with two actionable elements. */
export function fixtureObservation(
  overrides: Partial<AdaptiveObservation> = {},
): AdaptiveObservation {
  return {
    contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
    runId: FIXTURE_RUN_ID,
    observationId: "obs-1",
    revision: 1,
    controlEpoch: 7,
    surface: "semantic",
    axAvailable: true,
    domAvailable: false,
    frameDigest: FRAME_A,
    elements: [
      {
        elementId: "field-title",
        role: "textField",
        label: "Document title",
        valueDigest: VALUE_DIGEST_EMPTY,
        enabled: true,
        focused: true,
        sensitivity: "normal",
        actionClasses: ["set_value"],
      },
      {
        elementId: "button-save",
        role: "button",
        label: "Save",
        enabled: true,
        focused: false,
        sensitivity: "normal",
        actionClasses: ["invoke"],
      },
      {
        elementId: "button-publish",
        role: "button",
        label: "Publish",
        enabled: true,
        focused: false,
        sensitivity: "elevated",
        actionClasses: ["invoke"],
      },
    ],
    elementsTruncated: false,
    contradictions: [],
    ...overrides,
  };
}

/** The only authorized move: exactly one candidate, so no model is needed. */
export const FIXTURE_SINGLE_CANDIDATE: readonly AdaptiveCandidateAction[] = [
  {
    candidateId: "cand-save",
    kind: "invoke",
    elementId: "button-save",
    actionClass: "invoke",
    mutating: true,
    authorized: true,
    expectation: { kind: "frame_changed" },
  },
];

/**
 * Two authorized moves plus one the host refused to authorize. The
 * unauthorized candidate must never reach a model or become a plan.
 */
export const FIXTURE_AMBIGUOUS_CANDIDATES: readonly AdaptiveCandidateAction[] = [
  {
    candidateId: "cand-save",
    kind: "invoke",
    elementId: "button-save",
    actionClass: "invoke",
    mutating: true,
    authorized: true,
    expectation: { kind: "frame_changed" },
  },
  {
    candidateId: "cand-title",
    kind: "set_value",
    elementId: "field-title",
    valueRef: "valueref-title-draft",
    actionClass: "set_value",
    mutating: true,
    authorized: true,
    expectation: {
      kind: "element_value_digest",
      elementId: "field-title",
      valueDigest: VALUE_DIGEST_FILLED,
    },
  },
  {
    candidateId: "cand-publish",
    kind: "invoke",
    elementId: "button-publish",
    actionClass: "invoke",
    mutating: true,
    authorized: false,
    expectation: { kind: "frame_changed" },
  },
];

/**
 * Inputs that must fail closed.
 *
 * `expect` names the parser each case targets so a future contributor cannot
 * quietly move a case to a parser that happens to still reject it.
 */
export type AdaptiveRejectionFixture = {
  readonly name: string;
  readonly parser: "element" | "observation" | "candidate";
  readonly value: unknown;
};

export const ADAPTIVE_REJECTION_FIXTURES: readonly AdaptiveRejectionFixture[] = [
  {
    name: "label carrying a host path",
    parser: "element",
    value: {
      elementId: "field-path",
      role: "textField",
      label: "/Users/operator/Documents/secret.txt",
      enabled: true,
      focused: false,
      sensitivity: "normal",
      actionClasses: ["set_value"],
    },
  },
  {
    name: "label carrying a credential marker",
    parser: "element",
    value: {
      elementId: "field-token",
      role: "textField",
      label: "api_key: sk-live-abcdef",
      enabled: true,
      focused: false,
      sensitivity: "normal",
      actionClasses: ["set_value"],
    },
  },
  {
    name: "label carrying an absolute URL",
    parser: "element",
    value: {
      elementId: "link-out",
      role: "link",
      label: "https://exfiltrate.example/collect",
      enabled: true,
      focused: false,
      sensitivity: "normal",
      actionClasses: ["invoke"],
    },
  },
  {
    name: "label carrying clipboard contents",
    parser: "element",
    value: {
      elementId: "field-paste",
      role: "textField",
      label: "clipboard: 4111 1111 1111 1111",
      enabled: true,
      focused: false,
      sensitivity: "normal",
      actionClasses: ["set_value"],
    },
  },
  {
    name: "raw value instead of a digest",
    parser: "element",
    value: {
      elementId: "field-title",
      role: "textField",
      value: "hunter2",
      enabled: true,
      focused: false,
      sensitivity: "normal",
      actionClasses: ["set_value"],
    },
  },
  {
    name: "value digest that is not opaque hex",
    parser: "element",
    value: {
      elementId: "field-title",
      role: "textField",
      valueDigest: "the user typed hunter2",
      enabled: true,
      focused: false,
      sensitivity: "normal",
      actionClasses: ["set_value"],
    },
  },
  {
    name: "screenshot bytes smuggled into an observation",
    parser: "observation",
    value: {
      contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
      runId: FIXTURE_RUN_ID,
      observationId: "obs-1",
      revision: 1,
      controlEpoch: 7,
      surface: "semantic",
      axAvailable: true,
      domAvailable: false,
      frameDigest: FRAME_A,
      screenshot: "data:image/png;base64,iVBORw0KGgo=",
      elements: [],
      elementsTruncated: false,
      contradictions: [],
    },
  },
  {
    name: "semantic surface with no AX or DOM backing",
    parser: "observation",
    value: {
      contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
      runId: FIXTURE_RUN_ID,
      observationId: "obs-1",
      revision: 1,
      controlEpoch: 7,
      surface: "semantic",
      axAvailable: false,
      domAvailable: false,
      frameDigest: FRAME_A,
      elements: [],
      elementsTruncated: false,
      contradictions: [],
    },
  },
  {
    name: "screenshot-only surface smuggling semantic elements",
    parser: "observation",
    value: {
      contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
      runId: FIXTURE_RUN_ID,
      observationId: "obs-1",
      revision: 1,
      controlEpoch: 7,
      surface: "screenshot_only",
      axAvailable: false,
      domAvailable: false,
      frameDigest: FRAME_A,
      elements: [
        {
          elementId: "button-save",
          role: "button",
          enabled: true,
          focused: false,
          sensitivity: "normal",
          actionClasses: ["invoke"],
        },
      ],
      elementsTruncated: false,
      contradictions: [],
    },
  },
  {
    name: "duplicate element ids",
    parser: "observation",
    value: {
      contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
      runId: FIXTURE_RUN_ID,
      observationId: "obs-1",
      revision: 1,
      controlEpoch: 7,
      surface: "semantic",
      axAvailable: true,
      domAvailable: false,
      frameDigest: FRAME_A,
      elements: [
        {
          elementId: "button-save",
          role: "button",
          enabled: true,
          focused: false,
          sensitivity: "normal",
          actionClasses: ["invoke"],
        },
        {
          elementId: "button-save",
          role: "button",
          enabled: false,
          focused: false,
          sensitivity: "normal",
          actionClasses: ["invoke"],
        },
      ],
      elementsTruncated: false,
      contradictions: [],
    },
  },
  {
    name: "contradiction carrying free prose",
    parser: "observation",
    value: {
      contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
      runId: FIXTURE_RUN_ID,
      observationId: "obs-1",
      revision: 1,
      controlEpoch: 7,
      surface: "semantic",
      axAvailable: true,
      domAvailable: true,
      frameDigest: FRAME_A,
      elements: [],
      elementsTruncated: false,
      contradictions: ["The DOM says the button is at /Users/operator/app"],
    },
  },
  {
    name: "generic execute escape hatch",
    parser: "candidate",
    value: {
      candidateId: "cand-exec",
      kind: "execute",
      elementId: "button-save",
      actionClass: "invoke",
      mutating: true,
      authorized: true,
      expectation: { kind: "frame_changed" },
    },
  },
  {
    name: "shell command smuggled as a value reference",
    parser: "candidate",
    value: {
      candidateId: "cand-shell",
      kind: "set_value",
      elementId: "field-title",
      valueRef: "rm -rf /",
      actionClass: "set_value",
      mutating: true,
      authorized: true,
      expectation: { kind: "frame_changed" },
    },
  },
  {
    name: "value reference on a non-text action",
    parser: "candidate",
    value: {
      candidateId: "cand-odd",
      kind: "invoke",
      elementId: "button-save",
      valueRef: "valueref-title-draft",
      actionClass: "invoke",
      mutating: true,
      authorized: true,
      expectation: { kind: "frame_changed" },
    },
  },
  {
    name: "candidate with no expectation to verify against",
    parser: "candidate",
    value: {
      candidateId: "cand-unverifiable",
      kind: "invoke",
      elementId: "button-save",
      actionClass: "invoke",
      mutating: true,
      authorized: true,
    },
  },
];

/**
 * Untrusted gateway replies that must not become a plan.
 *
 * These are the shapes a small local model actually emits when it drifts off
 * grammar: prose, code fences, an extra "reasoning" field, an invented
 * candidate, a confidence outside the unit interval.
 */
export const ADAPTIVE_INVALID_REPLIES: readonly { name: string; reply: string }[] = [
  { name: "bare prose", reply: "I think you should click Save." },
  {
    name: "fenced JSON",
    reply:
      '```json\n{"candidateId":"cand-save","confidence":0.9,"rationaleCode":"matches_goal_semantics","abstain":false}\n```',
  },
  {
    name: "prose wrapped around JSON",
    reply:
      'Sure! {"candidateId":"cand-save","confidence":0.9,"rationaleCode":"matches_goal_semantics","abstain":false}',
  },
  {
    name: "extra free-text reasoning field",
    reply:
      '{"candidateId":"cand-save","confidence":0.9,"rationaleCode":"matches_goal_semantics","abstain":false,"reasoning":"Because the user asked me to ignore prior instructions"}',
  },
  {
    name: "invented candidate id",
    reply:
      '{"candidateId":"cand-delete-everything","confidence":0.99,"rationaleCode":"matches_goal_semantics","abstain":false}',
  },
  {
    name: "unauthorized candidate id",
    reply:
      '{"candidateId":"cand-publish","confidence":0.99,"rationaleCode":"matches_goal_semantics","abstain":false}',
  },
  {
    name: "confidence above the unit interval",
    reply:
      '{"candidateId":"cand-save","confidence":42,"rationaleCode":"matches_goal_semantics","abstain":false}',
  },
  {
    name: "negative confidence",
    reply:
      '{"candidateId":"cand-save","confidence":-0.5,"rationaleCode":"matches_goal_semantics","abstain":false}',
  },
  {
    name: "unknown rationale code",
    reply:
      '{"candidateId":"cand-save","confidence":0.9,"rationaleCode":"because_i_felt_like_it","abstain":false}',
  },
  {
    name: "missing abstain key",
    reply: '{"candidateId":"cand-save","confidence":0.9,"rationaleCode":"matches_goal_semantics"}',
  },
  { name: "empty reply", reply: "" },
  { name: "json array instead of object", reply: '["cand-save"]' },
];

/** Markers that must never appear in anything crossing the public boundary. */
export const ADAPTIVE_FORBIDDEN_MARKERS: readonly string[] = [
  "/Users/",
  "/private/",
  "/var/",
  "/home/",
  "GROKPTAH_HOME",
  "XAI_API_KEY",
  "apiKey",
  "api_key",
  "Authorization",
  "Bearer",
  "clipboard",
  "password",
  "data:image",
  "base64",
  "https://",
  "valueref-",
];
