export type HelpRetrievalFixture = {
  query: string;
  expectedId: string | null;
  topic?: "getting-started" | "providers" | "computer-use" | "operations" | "all";
  rationale: "exact" | "paraphrase" | "unsupported";
};

/**
 * Stable retrieval cases for comparing the offline scorer with a future
 * semantic index. The expected IDs are product contracts, not model output.
 */
export const HELP_RETRIEVAL_FIXTURES: HelpRetrievalFixture[] = [
  {
    query: "semantic search",
    expectedId: "getting-started.search",
    rationale: "exact",
  },
  {
    query: "find a build",
    expectedId: "getting-started.search",
    rationale: "paraphrase",
  },
  {
    query: "company gateway weaker model",
    expectedId: "providers.gateway",
    rationale: "paraphrase",
  },
  {
    query: "grok build quota receipt",
    expectedId: "providers.live-gateway-evidence",
    rationale: "paraphrase",
  },
  {
    query: "grok bot quota vs grok build",
    expectedId: "providers.grok-build-boundary",
    rationale: "paraphrase",
  },
  {
    query: "weak company model code review",
    expectedId: "providers.restricted-gateway-review",
    rationale: "paraphrase",
  },
  {
    query: "stale frame clicking",
    expectedId: "computer-use.boundaries",
    rationale: "paraphrase",
  },
  {
    query: "VM sandboxed desktop screen",
    expectedId: "computer-use.isolated-guest",
    rationale: "paraphrase",
  },
  {
    query: "two agents one screen",
    expectedId: "computer-use.multi-agent-coordination",
    rationale: "paraphrase",
  },
  {
    query: "72 hour persistent workers",
    expectedId: "operations.always-on-soak",
    rationale: "paraphrase",
  },
  {
    query: "reconnects credential rotations",
    expectedId: "operations.always-on-soak",
    rationale: "paraphrase",
  },
  {
    query: "cited AI help answer",
    expectedId: "operations.help-assistant",
    rationale: "paraphrase",
  },
  {
    query: "teleport my repository",
    expectedId: null,
    rationale: "unsupported",
  },
  {
    query: "search",
    topic: "computer-use",
    expectedId: null,
    rationale: "unsupported",
  },
];
