/**
 * Deterministic retrieval fixtures for the canonical Help authority.
 *
 * These are the contract: each case names the query, the article the shipped
 * corpus must rank first, and whether the retriever is expected to answer or
 * abstain. They are expectations about the corpus, not measurements of a
 * model, and they must be updated deliberately alongside the corpus digest.
 */

import type { HelpAbstainReason, HelpAuthorityAudience } from "./helpAuthority";
import type { HelpTopic } from "./helpCenter";

export type HelpAuthorityFixture = {
  readonly query: string;
  /** The article that must rank first, or null when the retriever abstains. */
  readonly expectedId: string | null;
  readonly expectedOutcome: "answer" | "abstain";
  readonly expectedAbstainReason?: HelpAbstainReason;
  readonly topic?: HelpTopic | "all";
  readonly audience?: HelpAuthorityAudience;
  readonly includeRestricted?: boolean;
  readonly rationale: "exact" | "paraphrase" | "filtered" | "unsupported";
};

export const HELP_AUTHORITY_FIXTURES: readonly HelpAuthorityFixture[] = Object.freeze([
  {
    query: "sessions builds chats",
    expectedId: "getting-started.sessions",
    expectedOutcome: "answer",
    rationale: "exact",
  },
  {
    query: "semantic search",
    expectedId: "getting-started.search",
    expectedOutcome: "answer",
    rationale: "exact",
  },
  {
    query: "find a build",
    expectedId: "getting-started.search",
    expectedOutcome: "answer",
    rationale: "paraphrase",
  },
  {
    query: "restricted company gateway",
    expectedId: "providers.restricted-gateway-review",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "exact",
  },
  {
    query: "weak company model code review",
    expectedId: "providers.restricted-gateway-review",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "provider profile route policy",
    expectedId: "providers.gateway",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "grok bot quota vs grok build",
    expectedId: "providers.grok-build-boundary",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "stale frame clicking",
    expectedId: "computer-use.boundaries",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "isolated guest VM",
    expectedId: "computer-use.isolated-guest",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "two agents one screen",
    expectedId: "computer-use.multi-agent-coordination",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "approve semantic action postcondition",
    expectedId: "computer-use.consent",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "recover interrupted run checkpoint",
    expectedId: "operations.durable-recovery",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "queue next prompt stale revision",
    expectedId: "operations.prompt-queue",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "review receipt changed files fingerprint",
    expectedId: "operations.review-receipts",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "MCP reconnect capability events",
    expectedId: "operations.mcp-coordination",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "72 hour persistent workers",
    expectedId: "operations.always-on-soak",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "persistent agent round budget",
    expectedId: "capability.persistent-agents",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "promote isolated review approval",
    expectedId: "capability.promotion-and-discard",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "keyboard screen reader focus",
    expectedId: "capability.power-user-accessibility",
    expectedOutcome: "answer",
    rationale: "paraphrase",
  },
  {
    query: "ContextDesk War Room browser broker",
    expectedId: "providers.browser-broker",
    expectedOutcome: "answer",
    rationale: "paraphrase",
  },
  {
    query: "spin up an isolated cloud coding agent",
    expectedId: "providers.external-cloud-workers",
    expectedOutcome: "answer",
    includeRestricted: true,
    rationale: "paraphrase",
  },
  {
    query: "cited AI help answer",
    expectedId: "operations.help-assistant",
    expectedOutcome: "answer",
    rationale: "paraphrase",
  },
  // A gated article is invisible to a public search even on an exact query.
  // Weak public articles still score, so the abstention is on confidence.
  {
    query: "isolated guest VM",
    expectedId: null,
    expectedOutcome: "abstain",
    expectedAbstainReason: "low-confidence",
    rationale: "filtered",
  },
  // The corpus documents no such feature, so the retriever must not guess.
  {
    query: "teleport my repository",
    expectedId: null,
    expectedOutcome: "abstain",
    expectedAbstainReason: "low-confidence",
    includeRestricted: true,
    rationale: "unsupported",
  },
  {
    query: "search",
    topic: "computer-use",
    expectedId: null,
    expectedOutcome: "abstain",
    expectedAbstainReason: "no-match",
    includeRestricted: true,
    rationale: "filtered",
  },
  {
    query: "zzzz qqqq",
    expectedId: null,
    expectedOutcome: "abstain",
    expectedAbstainReason: "no-match",
    includeRestricted: true,
    rationale: "unsupported",
  },
  {
    query: "   ",
    expectedId: null,
    expectedOutcome: "abstain",
    expectedAbstainReason: "empty-query",
    rationale: "unsupported",
  },
]);
