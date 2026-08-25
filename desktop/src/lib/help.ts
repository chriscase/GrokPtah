/**
 * Local, transport-neutral Help Center index.
 *
 * Search is deliberately deterministic and permission-safe: it ranks shipped
 * help content, but never grants authority or invents live capability state.
 * Consumers can reuse this module in the desktop app, a web broker, or a
 * future published UI package.
 */

import { PROJECTED_HELP_ENTRIES } from "./help/canonical/projections";

export const HELP_CONTRACT = "grokptah.help.v1" as const;

export type HelpAudience = "everyone" | "power_user" | "operator";
export type HelpAccess = "public" | "gated" | "operator";

export type HelpEntry = {
  readonly id: string;
  readonly title: string;
  readonly summary: string;
  readonly body: string;
  readonly tags: readonly string[];
  readonly keywords: readonly string[];
  readonly audience: readonly HelpAudience[];
  readonly access: HelpAccess;
  readonly capabilityIds: readonly string[];
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
  /** The serialized payload size in UTF-8 bytes, excluding these two fields. */
  contextBytes: number;
  maxBytes: number;
  truncated: boolean;
};

export type HelpAssistantOptions = HelpSearchOptions & {
  /** Maximum UTF-8 bytes for the serialized assistant payload. */
  maxBytes?: number;
};

export const HELP_ASSISTANT_MAX_BYTES = 16_384 as const;
const HELP_ASSISTANT_MIN_BYTES = 512;
const HELP_ASSISTANT_HARD_MAX_BYTES = 65_536;
const HELP_ASSISTANT_INSTRUCTION =
  "Explain only the supplied help entries. Do not claim live capability, approval, lease, quota, or authority state; require a fresh scoped check before suggesting an operation.";

/**
 * The shipped corpus is generated from the canonical Help corpus.
 *
 * `desktop/src/lib/help/canonical/data.ts` is the only hand-maintained corpus
 * in the tree; this contract keeps its published shape, ranking, and immutability
 * guarantees while its content is projected from that single source.
 */
export const HELP_ENTRIES: readonly HelpEntry[] = PROJECTED_HELP_ENTRIES;

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

function utf8Bytes(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

/** Truncate by UTF-8 bytes without splitting a Unicode code point. */
function truncateUtf8(value: string, maxBytes: number): string {
  if (utf8Bytes(value) <= maxBytes) return value;
  let result = "";
  for (const character of value) {
    const next = result + character;
    if (utf8Bytes(next) > maxBytes) break;
    result = next;
  }
  return result;
}

function boundedEntry(entry: HelpEntry): HelpEntry {
  return {
    ...entry,
    title: truncateUtf8(entry.title, 256),
    summary: truncateUtf8(entry.summary, 1_024),
    body: truncateUtf8(entry.body, 4_096),
    tags: entry.tags.slice(0, 16).map((tag) => truncateUtf8(tag, 128)),
    keywords: entry.keywords.slice(0, 24).map((keyword) => truncateUtf8(keyword, 128)),
    capabilityIds: entry.capabilityIds.slice(0, 16),
  };
}

function normalizedMaxBytes(value: number | undefined): number {
  if (!Number.isFinite(value)) return HELP_ASSISTANT_MAX_BYTES;
  return Math.max(
    HELP_ASSISTANT_MIN_BYTES,
    Math.min(value ?? HELP_ASSISTANT_MAX_BYTES, HELP_ASSISTANT_HARD_MAX_BYTES),
  );
}

function fitQueryToPayload(query: string, maxBytes: number, instruction: string): string {
  const payloadWithoutHits = (candidate: string) => JSON.stringify({
    contract: HELP_CONTRACT,
    query: candidate,
    hits: [],
    instruction,
  });
  if (utf8Bytes(payloadWithoutHits(query)) <= maxBytes) return query;
  let low = 0;
  let high = utf8Bytes(query);
  while (low < high) {
    const midpoint = Math.ceil((low + high) / 2);
    const candidate = truncateUtf8(query, midpoint);
    if (utf8Bytes(payloadWithoutHits(candidate)) <= maxBytes) {
      low = midpoint;
    } else {
      high = midpoint - 1;
    }
  }
  return truncateUtf8(query, low);
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
  options: HelpAssistantOptions = {},
): HelpAssistantContext {
  const maxBytes = normalizedMaxBytes(options.maxBytes);
  const instruction = HELP_ASSISTANT_INSTRUCTION;
  const boundedQuery = fitQueryToPayload(
    truncateUtf8(query.trim(), 512),
    maxBytes,
    instruction,
  );
  const hits = searchHelp(query, { ...options, limit: Math.min(options.limit ?? 5, 5) });
  const selected: Array<Pick<HelpSearchHit, "entry" | "matchedTerms">> = [];
  let truncated = boundedQuery !== query.trim();

  for (const hit of hits) {
    const candidate = {
      entry: boundedEntry(hit.entry),
      matchedTerms: hit.matchedTerms.slice(0, 24),
    };
    const payload = {
      contract: HELP_CONTRACT,
      query: boundedQuery,
      hits: [...selected, candidate],
      instruction,
    };
    if (utf8Bytes(JSON.stringify(payload)) > maxBytes) {
      truncated = true;
      continue;
    }
    selected.push(candidate);
  }

  const payload = {
    contract: HELP_CONTRACT,
    query: boundedQuery,
    hits: selected,
    instruction,
  };
  const contextBytes = utf8Bytes(JSON.stringify(payload));
  return { ...payload, contextBytes, maxBytes, truncated };
}
