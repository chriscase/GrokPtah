/**
 * Local, transport-neutral Help Center index.
 *
 * Search is deliberately deterministic and permission-safe: it ranks shipped
 * help content, but never grants authority or invents live capability state.
 * Consumers can reuse this module in the desktop app, a web broker, or a
 * future published UI package.
 */

export const HELP_CONTRACT = "grokptah.help.v1" as const;

export type HelpAudience = "everyone" | "power_user" | "operator";
export type HelpAccess = "public" | "gated" | "operator";

export type HelpEntry = {
  id: string;
  title: string;
  summary: string;
  body: string;
  tags: string[];
  keywords: string[];
  audience: HelpAudience[];
  access: HelpAccess;
  capabilityIds: string[];
};

export type HelpSearchOptions = {
  limit?: number;
  audience?: HelpAudience;
  capabilityIds?: string[];
  includeRestricted?: boolean;
};

export type HelpSearchHit = {
  entry: HelpEntry;
  score: number;
  matchedTerms: string[];
};

export type HelpAssistantContext = {
  contract: typeof HELP_CONTRACT;
  query: string;
  hits: Array<Pick<HelpSearchHit, "entry" | "matchedTerms">>;
  instruction: string;
};

/** The shipped corpus is intentionally small, explicit, and reviewable. */
export const HELP_ENTRIES: readonly HelpEntry[] = [
  {
    id: "capabilities-and-integrations",
    title: "Use GrokPtah from another project",
    summary:
      "Embed the versioned Rust and TypeScript capability contracts in a desktop app, web broker, or service.",
    body:
      "Start with the grokptah-agent-sdk contracts and the transport-neutral TypeScript clients. Consumers discover capabilities, scope every run, and preserve the same approval, replay, and redaction rules. The browser-safe broker keeps credentials server-side and exposes opaque run identifiers only.",
    tags: ["sdk", "integration", "broker", "cross-project"],
    keywords: ["embed", "reuse", "package", "contextdesk", "web", "api"],
    audience: ["everyone", "power_user", "operator"],
    access: "public",
    capabilityIds: ["run.observe", "run.execute", "review.read"],
  },
  {
    id: "durable-runs-and-recovery",
    title: "Recover a long-running agent safely",
    summary:
      "Durable runs survive restarts through explicit checkpoints, replay cursors, and continuation gates.",
    body:
      "A restart is not permission to resend. Resume only from a durable checkpoint with the same scoped run identity, idempotency key, and evidence of the last accepted transport attempt. Unknown delivery stays unknown until the reconciliation oracle proves whether a new send is allowed.",
    tags: ["durability", "restart", "recovery", "always-on"],
    keywords: ["soak", "resume", "checkpoint", "duplicate", "replay", "uncertain"],
    audience: ["everyone", "power_user", "operator"],
    access: "public",
    capabilityIds: ["run.observe", "run.retry"],
  },
  {
    id: "persistent-agents",
    title: "Operate persistent agents",
    summary:
      "Persistent agents have explicit ownership, bounded rounds, continuation plans, and terminal evidence.",
    body:
      "Create a persistent agent only with a declared workspace, owner, model policy, and maximum round budget. Inspect its plan before resuming it, and keep human approval in the loop for any mutating or computer-control capability. An agent is operationally complete only when its terminal evidence is durable and auditable.",
    tags: ["agents", "operations", "budgets", "continuation"],
    keywords: ["long running", "always on", "worker", "supervisor", "background"],
    audience: ["power_user", "operator"],
    access: "gated",
    capabilityIds: ["run.execute", "run.retry"],
  },
  {
    id: "computer-use-safety",
    title: "Use Computer Use without losing control",
    summary:
      "Computer Use is observation-first, semantic, lease-fenced, and fail-closed on stale or sensitive state.",
    body:
      "Observe a redacted semantic snapshot before every action. The action must name the observed target, revision, enabled state, and bounded action class. Lease expiry, stale observations, helper failure, or cleanup uncertainty deny control; raw global input and unredacted clipboard, credential, path, and network data are not part of the public contract.",
    tags: ["computer use", "safety", "leases", "redaction"],
    keywords: ["cu", "desktop", "vm", "isolated", "mouse", "keyboard", "screen"],
    audience: ["everyone", "power_user", "operator"],
    access: "gated",
    capabilityIds: ["computer.observe", "computer.control"],
  },
  {
    id: "approvals-and-permissions",
    title: "Understand approvals and permissions",
    summary:
      "A visible capability is not automatically executable; mutating and computer actions require their declared gate.",
    body:
      "Review the requested scope, target, risk, and expiry before approving. Deny or pause when the observed target changed. Approval is scoped to the current run and capability; it never silently expands to raw input, another workspace, another agent, or a future revision.",
    tags: ["permissions", "approval", "security", "scope"],
    keywords: ["consent", "human gate", "authorize", "deny", "risk"],
    audience: ["everyone", "power_user", "operator"],
    access: "public",
    capabilityIds: ["run.execute", "computer.control"],
  },
  {
    id: "enterprise-gateway-review",
    title: "Review code through a restricted company gateway",
    summary:
      "Use a permitted gateway for code review even when its model is weaker, while preserving evidence and policy boundaries.",
    body:
      "Select a provider profile that identifies the gateway, allowed routes, token scope, quota observability, and retry policy. Treat provider identity and quota truth as evidence, not assumptions. The review receipt records the exact repository, base/head, gateway route, model, and findings without copying credentials or account balances.",
    tags: ["enterprise", "gateway", "review", "audit"],
    keywords: ["company", "restricted", "weak model", "quota", "provider", "compliance"],
    audience: ["power_user", "operator"],
    access: "operator",
    capabilityIds: ["review.read", "review.receipt"],
  },
  {
    id: "help-search-and-assistant",
    title: "Search Help Center and ask for guidance",
    summary:
      "Search is local and deterministic; an optional assistant may explain results but cannot grant authority.",
    body:
      "Use natural language, capability names, or tags such as ‘stale frame’, ‘restricted gateway’, or ‘restart duplicate’. Search results are permission-safe help content. Any assistant context is bounded to those results and must re-check live capabilities, approvals, leases, and scope before suggesting an action.",
    tags: ["help", "search", "assistant", "accessibility"],
    keywords: ["semantic", "documentation", "how do I", "explain", "guide"],
    audience: ["everyone", "power_user", "operator"],
    access: "public",
    capabilityIds: [],
  },
  {
    id: "power-user-accessibility",
    title: "Work quickly with keyboard and screen reader",
    summary:
      "Power-user speed and accessibility are designed together: named landmarks, predictable focus, and visible state.",
    body:
      "Use the Sessions, Tools, Live, and Computer landmarks to move between work areas. Running state, permission requests, stale observations, and recovery actions must be announced and remain visible. Reduced motion and large text settings preserve the same information hierarchy rather than hiding status.",
    tags: ["accessibility", "keyboard", "screen reader", "ux"],
    keywords: ["focus", "shortcut", "contrast", "reduced motion", "large text"],
    audience: ["everyone", "power_user"],
    access: "public",
    capabilityIds: [],
  },
];

const SYNONYMS: Record<string, string[]> = {
  agent: ["agents", "worker", "persistent"],
  computer: ["computer use", "cu", "desktop", "vm"],
  gateway: ["provider", "enterprise", "company", "restricted"],
  help: ["guide", "documentation", "assistant", "search"],
  restart: ["recovery", "resume", "continuation"],
  stale: ["revision", "observation", "frame"],
};

function terms(value: string): string[] {
  return value
    .toLocaleLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, " ")
    .split(/\s+/)
    .filter((term) => term.length > 1);
}

function expandedTerms(query: string): string[] {
  const queryTerms = terms(query);
  return [...new Set(queryTerms.flatMap((term) => [term, ...(SYNONYMS[term] ?? [])]))];
}

function searchableText(entry: HelpEntry): Record<string, string> {
  return {
    title: entry.title.toLocaleLowerCase(),
    summary: entry.summary.toLocaleLowerCase(),
    body: entry.body.toLocaleLowerCase(),
    tags: entry.tags.join(" ").toLocaleLowerCase(),
    keywords: entry.keywords.join(" ").toLocaleLowerCase(),
  };
}

function scoreEntry(query: string, entry: HelpEntry): HelpSearchHit | null {
  const queryTerms = expandedTerms(query);
  if (queryTerms.length === 0) return null;
  const text = searchableText(entry);
  let score = 0;
  const matchedTerms: string[] = [];
  for (const term of queryTerms) {
    let weight = 0;
    if (text.title.includes(term)) weight = Math.max(weight, 12);
    if (text.tags.includes(term)) weight = Math.max(weight, 8);
    if (text.keywords.includes(term)) weight = Math.max(weight, 6);
    if (text.summary.includes(term)) weight = Math.max(weight, 4);
    if (text.body.includes(term)) weight = Math.max(weight, 2);
    if (weight > 0) {
      score += weight;
      if (!matchedTerms.includes(term)) matchedTerms.push(term);
    }
  }
  if (score === 0) return null;
  if (text.title.includes(query.toLocaleLowerCase().trim())) score += 10;
  return { entry, score, matchedTerms };
}

/** Search the local corpus with deterministic synonym-aware ranking. */
export function searchHelp(
  query: string,
  options: HelpSearchOptions = {},
): HelpSearchHit[] {
  const limit = Math.max(1, Math.min(options.limit ?? 8, 50));
  const requestedCapabilities = new Set(options.capabilityIds ?? []);
  return HELP_ENTRIES.map((entry) => scoreEntry(query, entry))
    .filter((hit): hit is HelpSearchHit => hit !== null)
    .filter(({ entry }) =>
      options.includeRestricted || entry.access === "public",
    )
    .filter(({ entry }) =>
      !options.audience || entry.audience.includes(options.audience),
    )
    .filter(({ entry }) =>
      requestedCapabilities.size === 0 ||
      entry.capabilityIds.some((id) => requestedCapabilities.has(id)),
    )
    .sort((a, b) => b.score - a.score || a.entry.title.localeCompare(b.entry.title))
    .slice(0, limit);
}

/**
 * Build bounded context for an optional help assistant.
 *
 * This is explanatory context only. Consumers must perform a fresh live
 * capability/approval/lease check before invoking any operation.
 */
export function buildHelpAssistantContext(
  query: string,
  options: HelpSearchOptions = {},
): HelpAssistantContext {
  const hits = searchHelp(query, { ...options, limit: Math.min(options.limit ?? 5, 5) });
  return {
    contract: HELP_CONTRACT,
    query: query.trim(),
    hits: hits.map(({ entry, matchedTerms }) => ({ entry, matchedTerms })),
    instruction:
      "Explain only the supplied help entries. Do not claim live capability, approval, lease, quota, or authority state; require a fresh scoped check before suggesting an operation.",
  };
}
