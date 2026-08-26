/**
 * Canonical, source-cited Help authority.
 *
 * The repository previously admitted two divergent Help corpora: the
 * capability-aware `help.ts` entries and the source-cited `helpCenter.ts`
 * articles. Neither alone satisfies a reusable Help contract — one carries
 * audience/capability metadata without citations, the other carries citations
 * without audience/capability metadata, neither is versioned by digest, and
 * the two overlap on several subjects with no declared relationship.
 *
 * This module folds both into one canonical corpus: one article per subject,
 * carrying every contributing corpus's text as a separately-cited passage, so
 * unification loses neither guidance nor provenance. On top of it sits the
 * retrieval contract GrokPtah and external products such as ContextDesk need:
 * offline hybrid lexical+token search, deterministic ranking, per-hit
 * explanations, exact citation spans, and explicit abstention.
 *
 * Authority boundary: this module is retrieval over shipped documentation.
 * It reads no workspace, session, provider, credential, or native state, it
 * grants no capability, it performs no I/O, and it is Tauri-free so the
 * browser/public bundles can consume it. It answers "what does the shipped
 * documentation say", never "what may this caller do right now".
 */

import { HELP_ENTRIES, type HelpEntry } from "./help";
import {
  HELP_ARTICLES,
  HELP_CORPUS_VERSION,
  type HelpArticle,
  type HelpSource,
  type HelpTopic,
} from "./helpCenter";

export const HELP_AUTHORITY_CONTRACT = "grokptah.help-authority.v1" as const;

/** Canonical corpus version. Bump together with the recorded digest. */
export const HELP_AUTHORITY_CORPUS_VERSION = "help-authority-v1" as const;

/** Upstream corpora folded into the canonical corpus, kept as provenance. */
export const HELP_SOURCE_CORPORA = ["grokptah.help.v1", "product-corpus-v1"] as const;
export type HelpCorpusId = (typeof HELP_SOURCE_CORPORA)[number];

/** Short, stable slug per corpus, used to build passage identifiers. */
const CORPUS_SLUG: Readonly<Record<HelpCorpusId, string>> = Object.freeze({
  "product-corpus-v1": "product",
  "grokptah.help.v1": "capability",
});

export type HelpAuthorityAudience = "everyone" | "power_user" | "operator";
export type HelpAuthorityAccess = "public" | "gated" | "operator";

/** Restrictiveness order. A merged article takes the most restrictive value. */
const ACCESS_RANK: Readonly<Record<HelpAuthorityAccess, number>> = Object.freeze({
  public: 0, gated: 1, operator: 2,
});

/** Where a canonical article's content came from, so provenance survives. */
export type HelpProvenance = {
  readonly corpus: HelpCorpusId;
  /** The article's identifier in its originating corpus. */
  readonly sourceArticleId: string;
};

/**
 * One contributing corpus's prose for a canonical article.
 *
 * Passages, not articles, are the unit of citation: each carries the exact
 * sources backing its own text, so a span quoted from it can name what
 * documents it.
 */
export type HelpPassage = {
  /** Stable `${articleId}#${corpusSlug}` identifier. */
  readonly id: string;
  readonly corpus: HelpCorpusId;
  readonly sourceArticleId: string;
  readonly text: string;
  readonly sources: readonly HelpSource[];
};

export type HelpAuthorityArticle = {
  readonly id: string;
  readonly title: string;
  readonly topic: HelpTopic;
  readonly summary: string;
  /** At least one passage, ordered deterministically by passage ID. */
  readonly passages: readonly HelpPassage[];
  readonly aliases: readonly string[];
  readonly keywords: readonly string[];
  readonly audience: readonly HelpAuthorityAudience[];
  readonly access: HelpAuthorityAccess;
  readonly capabilityIds: readonly string[];
  /** Deduplicated union of every passage's sources. */
  readonly sources: readonly HelpSource[];
  /** One record per contributing corpus. */
  readonly provenance: readonly HelpProvenance[];
};

/** Every key a canonical article may carry. Unknown keys fail closed. */
export const HELP_ARTICLE_KEYS: readonly string[] = Object.freeze([
  "id", "title", "topic", "summary", "passages", "aliases", "keywords",
  "audience", "access", "capabilityIds", "sources", "provenance",
]);

const HELP_PASSAGE_KEYS: readonly string[] = Object.freeze([
  "id", "corpus", "sourceArticleId", "text", "sources",
]);
const HELP_SOURCE_KEYS: readonly string[] = Object.freeze(["id", "path", "heading"]);
const HELP_PROVENANCE_KEYS: readonly string[] = Object.freeze(["corpus", "sourceArticleId"]);

const HELP_TOPICS: readonly HelpTopic[] = Object.freeze([
  "getting-started", "providers", "computer-use", "operations",
]);
const HELP_AUDIENCES: readonly HelpAuthorityAudience[] = Object.freeze([
  "everyone", "power_user", "operator",
]);
const HELP_ACCESS_LEVELS: readonly HelpAuthorityAccess[] = Object.freeze([
  "public", "gated", "operator",
]);

/** Bounds. Every one of these is a fail-closed ceiling, not a hint. */
export const HELP_MAX_QUERY_CHARS = 512;
// Reachable on purpose: 512 UTF-16 units of 3-byte text is 1536 bytes, so a
// ceiling above that could never fire and would be dead validation.
export const HELP_MAX_QUERY_BYTES = 1_024;
export const HELP_MAX_RESULTS = 25;
export const HELP_DEFAULT_RESULTS = 8;
export const HELP_MAX_SPANS_PER_HIT = 8;
export const HELP_MAX_SPAN_CHARS = 240;
export const HELP_MAX_EXPLANATION_SIGNALS = 24;
export const HELP_MAX_ID_CHARS = 128;
export const HELP_MAX_TITLE_CHARS = 256;
export const HELP_MAX_SUMMARY_CHARS = 1_024;
export const HELP_MAX_PASSAGE_CHARS = 8_192;
export const HELP_MAX_PASSAGES_PER_ARTICLE = 4;
export const HELP_MAX_SOURCE_PATH_CHARS = 256;

/** Below this the top hit is not offered as an answer. */
export const HELP_MIN_CONFIDENCE = 0.18;
/** At or above this a top hit is unambiguous even with a close runner-up. */
export const HELP_CLEAR_CONFIDENCE = 0.55;
/** A runner-up this close to the leader makes an unclear result ambiguous. */
export const HELP_AMBIGUITY_RATIO = 0.98;

/**
 * Audience/capability metadata for the source-cited product corpus.
 *
 * The product corpus ships citations but no audience or capability metadata.
 * This overlay supplies it explicitly rather than inferring it from prose, so
 * every value is reviewable in one place. It is deliberately exhaustive: an
 * article with no overlay entry fails assembly instead of silently defaulting
 * to the most permissive audience.
 */
const PRODUCT_CORPUS_METADATA: Readonly<Record<string, {
  audience: readonly HelpAuthorityAudience[];
  access: HelpAuthorityAccess;
  capabilityIds: readonly string[];
}>> = Object.freeze({
  "getting-started.sessions": {
    audience: ["everyone", "power_user", "operator"], access: "public",
    capabilityIds: ["session.observe"],
  },
  "getting-started.search": {
    audience: ["everyone", "power_user", "operator"], access: "public",
    capabilityIds: ["session.observe"],
  },
  "providers.gateway": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["run.execute"],
  },
  "providers.live-gateway-evidence": {
    audience: ["power_user", "operator"], access: "operator",
    capabilityIds: ["run.review"],
  },
  "providers.grok-build-boundary": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["run.execute"],
  },
  "providers.restricted-gateway-review": {
    audience: ["power_user", "operator"], access: "operator",
    capabilityIds: ["run.review"],
  },
  "providers.browser-broker": {
    audience: ["everyone", "power_user", "operator"], access: "public",
    capabilityIds: ["session.observe", "run.execute"],
  },
  "providers.external-cloud-workers": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["run.execute", "run.review"],
  },
  "computer-use.boundaries": {
    audience: ["everyone", "power_user", "operator"], access: "gated",
    capabilityIds: ["computer.observe", "computer.control"],
  },
  "computer-use.isolated-guest": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["computer.observe"],
  },
  "computer-use.multi-agent-coordination": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["computer.control"],
  },
  "computer-use.consent": {
    audience: ["everyone", "power_user", "operator"], access: "gated",
    capabilityIds: ["computer.control"],
  },
  "operations.evidence": {
    audience: ["everyone", "power_user", "operator"], access: "public",
    capabilityIds: [],
  },
  "operations.help-assistant": {
    audience: ["everyone", "power_user", "operator"], access: "public",
    capabilityIds: [],
  },
  "operations.always-on-soak": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["agent.continuity", "agent.resume"],
  },
  "operations.durable-recovery": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["run.execute", "run.review"],
  },
  "operations.prompt-queue": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["run.queue"],
  },
  "operations.review-receipts": {
    audience: ["power_user", "operator"], access: "operator",
    capabilityIds: ["run.promote", "run.review"],
  },
  "operations.mcp-coordination": {
    audience: ["power_user", "operator"], access: "gated",
    capabilityIds: ["session.observe"],
  },
});

/**
 * Topic and source citations for the capability-aware corpus.
 *
 * The capability corpus ships audience/capability metadata but cites no
 * sources, so its prose cannot be quoted with attribution on its own. Each
 * entry below names the shipped document and heading its guidance is drawn
 * from; a test resolves every heading against the real file, so these are
 * checked citations rather than plausible-looking ones.
 */
const CAPABILITY_CORPUS_METADATA: Readonly<Record<string, {
  topic: HelpTopic;
  sources: readonly HelpSource[];
}>> = Object.freeze({
  "capabilities-and-integrations": {
    topic: "providers",
    sources: [
      { id: "embedding.trust-boundary", path: "docs/EMBEDDING.md", heading: "Choose the trust boundary first" },
      { id: "embedding.ui-core", path: "docs/EMBEDDING.md", heading: "Headless UI primitives" },
    ],
  },
  "durable-runs-and-recovery": {
    topic: "operations",
    sources: [{ id: "durable.runs", path: "docs/DURABLE_RUNS.md", heading: "Lifecycle" }],
  },
  "persistent-agents": {
    topic: "operations",
    sources: [
      { id: "persistent.boundary", path: "docs/PERSISTENT_AGENT_PROTOCOL.md", heading: "Boundary" },
      { id: "persistent.lifecycle", path: "docs/PERSISTENT_AGENT_PROTOCOL.md", heading: "Lifecycle rules" },
    ],
  },
  "computer-use-safety": {
    topic: "computer-use",
    sources: [
      { id: "computer-use.overview", path: "docs/COMPUTER_USE.md", heading: "Safety boundary" },
      { id: "computer-use.threat-model", path: "docs/COMPUTER_USE_THREAT_MODEL.md", heading: "Trust boundaries" },
    ],
  },
  "approvals-and-permissions": {
    topic: "operations",
    sources: [
      { id: "computer-use.proposal-boundary", path: "docs/COMPUTER_USE.md", heading: "Model proposal boundary" },
    ],
  },
  "queue-and-steering": {
    topic: "operations",
    sources: [{ id: "durable.queue", path: "docs/MCP_CONTROL_COORDINATOR.md", heading: "Queue" }],
  },
  "promotion-and-discard": {
    topic: "operations",
    sources: [
      { id: "review.protocol", path: "docs/MCP_CONTROL_COORDINATOR.md", heading: "Evidence-backed handoff" },
    ],
  },
  "enterprise-gateway-review": {
    topic: "providers",
    sources: [{ id: "provider.profiles", path: "docs/PROVIDER_PROFILES.md", heading: "Provider profiles" }],
  },
  "help-search-and-assistant": {
    topic: "getting-started",
    sources: [{ id: "product.readme", path: "README.md", heading: "Features (desktop)" }],
  },
  "power-user-accessibility": {
    topic: "getting-started",
    sources: [{ id: "product.readme", path: "README.md", heading: "Features (desktop)" }],
  },
});

/**
 * Declared cross-corpus overlap: capability entry -> canonical product article.
 *
 * These pairs cover the same subject in both corpora, so they become one
 * canonical article with two cited passages rather than two articles that
 * compete for the same query. The mapping is deliberately conservative — an
 * entry is merged only where both corpora clearly document the same thing,
 * and the entries left out below stay as distinct articles:
 *
 *   persistent-agents        agent ownership and rounds, not soak evidence
 *   approvals-and-permissions the cross-capability approval model, not the
 *                             Computer-Use-specific action approval
 *   promotion-and-discard    promoting or discarding, not reading a receipt
 *   power-user-accessibility no product-corpus counterpart at all
 */
const CAPABILITY_MERGE_TARGET: Readonly<Record<string, string>> = Object.freeze({
  "capabilities-and-integrations": "providers.browser-broker",
  "durable-runs-and-recovery": "operations.durable-recovery",
  "queue-and-steering": "operations.prompt-queue",
  "computer-use-safety": "computer-use.boundaries",
  "help-search-and-assistant": "operations.help-assistant",
  "enterprise-gateway-review": "providers.restricted-gateway-review",
});

/** Canonical ID prefix for capability entries with no product counterpart. */
const CAPABILITY_ID_PREFIX = "capability.";

/* ------------------------------------------------------------------ *
 * Link safety
 * ------------------------------------------------------------------ */

export type HelpLinkRejection =
  | "empty"
  | "too-long"
  | "control-characters"
  | "whitespace"
  | "unsafe-scheme"
  | "protocol-relative"
  | "absolute-path"
  | "path-traversal"
  | "backslash";

export type HelpLinkCheck =
  | { safe: true; kind: "repo-relative" | "https" }
  | { safe: false; reason: HelpLinkRejection };

/** Schemes that must never reach a renderer or a fetcher from Help data. */
const UNSAFE_SCHEME = /^\s*(javascript|data|vbscript|file|blob|about|jar|ms-msdt)\s*:/i;
const ANY_SCHEME = /^[a-z0-9.+-]+:/i;
const WINDOWS_DRIVE = /^[a-z]:/i;

/**
 * C0/C1 controls, bidi and zero-width marks, and the BOM.
 *
 * Written as a code-point scan rather than a character class so the rejected
 * set stays readable in review instead of hiding inside a literal.
 */
function hasControlCharacters(value: string): boolean {
  for (const character of value) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20) return true;
    if (code >= 0x7f && code <= 0x9f) return true;
    if (code >= 0x200b && code <= 0x200f) return true;
    if (code >= 0x2028 && code <= 0x202e) return true;
    if (code >= 0x2066 && code <= 0x2069) return true;
    if (code === 0xfeff) return true;
  }
  return false;
}

/**
 * Accept only a repo-relative documentation path or an absolute https URL.
 *
 * Help data is rendered and, for a source path, resolved against the
 * repository. A scheme-bearing or traversing link is rejected outright rather
 * than sanitised, so a malformed corpus never becomes a navigable link.
 */
export function checkHelpLink(value: string): HelpLinkCheck {
  if (typeof value !== "string" || value.length === 0) {
    return { safe: false, reason: "empty" };
  }
  if (value.length > HELP_MAX_SOURCE_PATH_CHARS) {
    return { safe: false, reason: "too-long" };
  }
  if (hasControlCharacters(value)) return { safe: false, reason: "control-characters" };
  if (/\s/.test(value)) return { safe: false, reason: "whitespace" };
  if (value.includes("\\")) return { safe: false, reason: "backslash" };
  if (UNSAFE_SCHEME.test(value)) return { safe: false, reason: "unsafe-scheme" };
  if (value.startsWith("//")) return { safe: false, reason: "protocol-relative" };
  if (/^https:\/\//i.test(value)) return { safe: true, kind: "https" };
  // A drive letter is checked before the generic scheme rule: "C:/x" is
  // syntactically also a one-letter URI scheme, and the accurate reason to
  // reject it is that it is an absolute host path.
  if (value.startsWith("/") || WINDOWS_DRIVE.test(value)) {
    return { safe: false, reason: "absolute-path" };
  }
  if (ANY_SCHEME.test(value)) return { safe: false, reason: "unsafe-scheme" };
  if (value.split("/").includes("..")) return { safe: false, reason: "path-traversal" };
  return { safe: true, kind: "repo-relative" };
}

/** Reject prose that smuggles an executable-scheme link into rendered text. */
function containsUnsafeInlineLink(text: string): boolean {
  return /(?:^|[\s("'<[])(?:javascript|data|vbscript|file|blob)\s*:/i.test(text);
}

/* ------------------------------------------------------------------ *
 * Fail-closed corpus validation
 * ------------------------------------------------------------------ */

export type HelpCorpusIssueCode =
  | "not-an-object"
  | "unknown-field"
  | "missing-field"
  | "invalid-id"
  | "duplicate-id"
  | "invalid-topic"
  | "invalid-access"
  | "invalid-audience"
  | "empty-audience"
  | "invalid-capability-id"
  | "empty-text"
  | "oversized-text"
  | "no-passages"
  | "too-many-passages"
  | "duplicate-passage-id"
  | "no-sources"
  | "duplicate-source-id"
  | "unsafe-link"
  | "invalid-provenance"
  | "duplicate-provenance"
  | "provenance-mismatch";

export type HelpCorpusIssue = {
  readonly code: HelpCorpusIssueCode;
  /** Canonical article ID when known, otherwise the array index as text. */
  readonly articleId: string;
  readonly detail: string;
};

export type HelpCorpusValidation = {
  readonly valid: boolean;
  readonly issues: readonly HelpCorpusIssue[];
};

const ARTICLE_ID = /^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)+$/;
const CAPABILITY_ID = /^[a-z][a-z0-9]*(\.[a-z][a-z0-9_]*)+$/;
const SOURCE_ID = /^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function validateSources(
  value: unknown,
  where: string,
  add: (code: HelpCorpusIssueCode, detail: string) => void,
): void {
  if (!Array.isArray(value) || value.length === 0) {
    add("no-sources", `${where} must cite at least one source`);
    return;
  }
  const sourceIds = new Set<string>();
  for (const source of value) {
    if (!isRecord(source)) {
      add("not-an-object", `${where} source is not an object`);
      continue;
    }
    for (const key of Object.keys(source)) {
      if (!HELP_SOURCE_KEYS.includes(key)) add("unknown-field", `${where}.sources.${key}`);
    }
    if (typeof source.id !== "string" || !SOURCE_ID.test(source.id)) {
      add("invalid-id", `${where} source ${String(source.id)}`);
    } else if (sourceIds.has(source.id)) {
      add("duplicate-source-id", `${where}: ${source.id}`);
    } else {
      sourceIds.add(source.id);
    }
    if (typeof source.heading !== "string" || source.heading.trim().length === 0) {
      add("empty-text", `${where} source heading`);
    }
    const path = typeof source.path === "string" ? source.path : "";
    const link = checkHelpLink(path);
    if (!link.safe) add("unsafe-link", `${where}: ${path || "<empty>"}: ${link.reason}`);
  }
}

/**
 * Validate a candidate corpus and report every reason it fails.
 *
 * Callers treat a non-empty issue list as fatal. Validation reports all the
 * issues rather than only the first, so a corpus review sees the whole
 * picture, but a partially valid corpus is never usable.
 */
export function validateHelpAuthorityCorpus(articles: unknown): HelpCorpusValidation {
  const issues: HelpCorpusIssue[] = [];
  if (!Array.isArray(articles)) {
    return {
      valid: false,
      issues: Object.freeze([
        { code: "not-an-object" as const, articleId: "<corpus>", detail: "corpus is not an array" },
      ]),
    };
  }
  const seenIds = new Set<string>();
  const seenProvenance = new Set<string>();

  articles.forEach((candidate, index) => {
    const label = isRecord(candidate) && typeof candidate.id === "string"
      ? candidate.id
      : `#${index}`;
    const add = (code: HelpCorpusIssueCode, detail: string) => {
      issues.push({ code, articleId: label, detail });
    };

    if (!isRecord(candidate)) {
      add("not-an-object", "article is not an object");
      return;
    }
    for (const key of Object.keys(candidate)) {
      if (!HELP_ARTICLE_KEYS.includes(key)) add("unknown-field", key);
    }
    for (const key of HELP_ARTICLE_KEYS) {
      if (!(key in candidate)) add("missing-field", key);
    }

    if (typeof candidate.id !== "string" || !ARTICLE_ID.test(candidate.id) ||
        candidate.id.length > HELP_MAX_ID_CHARS) {
      add("invalid-id", String(candidate.id));
    } else if (seenIds.has(candidate.id)) {
      add("duplicate-id", candidate.id);
    } else {
      seenIds.add(candidate.id);
    }

    if (typeof candidate.topic !== "string" ||
        !HELP_TOPICS.includes(candidate.topic as HelpTopic)) {
      add("invalid-topic", String(candidate.topic));
    }
    if (typeof candidate.access !== "string" ||
        !HELP_ACCESS_LEVELS.includes(candidate.access as HelpAuthorityAccess)) {
      add("invalid-access", String(candidate.access));
    }

    if (!isStringArray(candidate.audience)) {
      add("invalid-audience", "audience is not a string array");
    } else if (candidate.audience.length === 0) {
      add("empty-audience", "an article must name at least one audience");
    } else {
      for (const value of candidate.audience) {
        if (!HELP_AUDIENCES.includes(value as HelpAuthorityAudience)) {
          add("invalid-audience", value);
        }
      }
    }

    if (!isStringArray(candidate.capabilityIds)) {
      add("invalid-capability-id", "capabilityIds is not a string array");
    } else {
      for (const value of candidate.capabilityIds) {
        if (!CAPABILITY_ID.test(value)) add("invalid-capability-id", value);
      }
    }

    for (const [field, max] of [
      ["title", HELP_MAX_TITLE_CHARS],
      ["summary", HELP_MAX_SUMMARY_CHARS],
    ] as ReadonlyArray<readonly [string, number]>) {
      const value = candidate[field];
      if (typeof value !== "string" || value.trim().length === 0) {
        add("empty-text", field);
        continue;
      }
      if (value.length > max) add("oversized-text", `${field} (${value.length} > ${max})`);
      if (containsUnsafeInlineLink(value)) {
        add("unsafe-link", `${field} contains an unsafe scheme`);
      }
    }
    for (const field of ["aliases", "keywords"] as const) {
      if (!isStringArray(candidate[field])) add("empty-text", field);
    }

    const declaredProvenance = new Set<string>();
    if (!Array.isArray(candidate.provenance) || candidate.provenance.length === 0) {
      add("invalid-provenance", "provenance is missing or empty");
    } else {
      for (const record of candidate.provenance) {
        if (!isRecord(record)) {
          add("invalid-provenance", "provenance record is not an object");
          continue;
        }
        for (const key of Object.keys(record)) {
          if (!HELP_PROVENANCE_KEYS.includes(key)) add("unknown-field", `provenance.${key}`);
        }
        const corpus = record.corpus;
        const sourceArticleId = record.sourceArticleId;
        if (typeof corpus !== "string" ||
            !(HELP_SOURCE_CORPORA as readonly string[]).includes(corpus)) {
          add("invalid-provenance", `corpus ${String(corpus)}`);
          continue;
        }
        if (typeof sourceArticleId !== "string" || sourceArticleId.trim().length === 0) {
          add("invalid-provenance", "sourceArticleId is empty");
          continue;
        }
        declaredProvenance.add(`${corpus}::${sourceArticleId}`);
        const key = `${corpus}::${sourceArticleId}`;
        if (seenProvenance.has(key)) add("duplicate-provenance", key);
        else seenProvenance.add(key);
      }
    }

    if (!Array.isArray(candidate.passages) || candidate.passages.length === 0) {
      add("no-passages", "an article must carry at least one passage");
    } else if (candidate.passages.length > HELP_MAX_PASSAGES_PER_ARTICLE) {
      add("too-many-passages", String(candidate.passages.length));
    } else {
      const passageIds = new Set<string>();
      for (const passage of candidate.passages) {
        if (!isRecord(passage)) {
          add("not-an-object", "passage is not an object");
          continue;
        }
        for (const key of Object.keys(passage)) {
          if (!HELP_PASSAGE_KEYS.includes(key)) add("unknown-field", `passages.${key}`);
        }
        if (typeof passage.id !== "string" || passage.id.trim().length === 0 ||
            passage.id.length > HELP_MAX_ID_CHARS) {
          add("invalid-id", `passage ${String(passage.id)}`);
        } else if (passageIds.has(passage.id)) {
          add("duplicate-passage-id", passage.id);
        } else {
          passageIds.add(passage.id);
        }
        if (typeof passage.text !== "string" || passage.text.trim().length === 0) {
          add("empty-text", "passage text");
        } else {
          if (passage.text.length > HELP_MAX_PASSAGE_CHARS) {
            add("oversized-text", `passage (${passage.text.length} > ${HELP_MAX_PASSAGE_CHARS})`);
          }
          if (containsUnsafeInlineLink(passage.text)) {
            add("unsafe-link", "passage text contains an unsafe scheme");
          }
        }
        const corpus = passage.corpus;
        const sourceArticleId = passage.sourceArticleId;
        if (typeof corpus !== "string" ||
            !(HELP_SOURCE_CORPORA as readonly string[]).includes(corpus)) {
          add("invalid-provenance", `passage corpus ${String(corpus)}`);
        } else if (typeof sourceArticleId === "string" &&
            !declaredProvenance.has(`${corpus}::${sourceArticleId}`)) {
          // A passage whose origin the article does not declare would be
          // uncitable: the reader could not tell where the text came from.
          add("provenance-mismatch", `${corpus}::${String(sourceArticleId)}`);
        }
        validateSources(passage.sources, `passage ${String(passage.id)}`, add);
      }
    }

    validateSources(candidate.sources, "article", add);
  });

  return { valid: issues.length === 0, issues: Object.freeze(issues) };
}

/* ------------------------------------------------------------------ *
 * Canonical corpus assembly
 * ------------------------------------------------------------------ */

function frozenStrings(values: readonly string[]): readonly string[] {
  return Object.freeze([...values]);
}

function freezeSources(sources: readonly HelpSource[]): readonly HelpSource[] {
  return Object.freeze(sources.map((source) => Object.freeze({ ...source })));
}

/** Union sources by identity, preserving first-seen order. */
function unionSources(...groups: ReadonlyArray<readonly HelpSource[]>): HelpSource[] {
  const seen = new Map<string, HelpSource>();
  for (const group of groups) {
    for (const source of group) {
      const key = `${source.id}::${source.path}::${source.heading}`;
      if (!seen.has(key)) seen.set(key, { ...source });
    }
  }
  return [...seen.values()];
}

/** Union strings case-insensitively, preserving first-seen spelling. */
function unionStrings(...groups: ReadonlyArray<readonly string[]>): string[] {
  const seen = new Map<string, string>();
  for (const group of groups) {
    for (const value of group) {
      const key = value.toLocaleLowerCase("en-US");
      if (!seen.has(key)) seen.set(key, value);
    }
  }
  return [...seen.values()];
}

function passageId(articleId: string, corpus: HelpCorpusId): string {
  return `${articleId}#${CORPUS_SLUG[corpus]}`;
}

type ArticleDraft = {
  id: string;
  title: string;
  topic: HelpTopic;
  summary: string;
  passages: HelpPassage[];
  aliases: string[];
  keywords: string[];
  audience: HelpAuthorityAudience[];
  access: HelpAuthorityAccess;
  capabilityIds: string[];
  provenance: HelpProvenance[];
};

function draftFromProductArticle(article: HelpArticle): ArticleDraft {
  const overlay = PRODUCT_CORPUS_METADATA[article.id];
  if (!overlay) {
    throw new Error(
      `help authority: product article ${article.id} has no audience/capability metadata`,
    );
  }
  return {
    id: article.id,
    title: article.title,
    topic: article.topic,
    summary: article.summary,
    passages: [{
      id: passageId(article.id, "product-corpus-v1"),
      corpus: "product-corpus-v1",
      sourceArticleId: article.id,
      text: article.body,
      sources: freezeSources(article.sources),
    }],
    aliases: [...article.aliases],
    keywords: [...article.keywords],
    audience: [...overlay.audience],
    access: overlay.access,
    capabilityIds: [...overlay.capabilityIds],
    provenance: [{ corpus: "product-corpus-v1", sourceArticleId: article.id }],
  };
}

function capabilityPassage(entry: HelpEntry, articleId: string): HelpPassage {
  const overlay = CAPABILITY_CORPUS_METADATA[entry.id];
  if (!overlay) {
    throw new Error(`help authority: capability entry ${entry.id} has no topic/source metadata`);
  }
  return {
    id: passageId(articleId, "grokptah.help.v1"),
    corpus: "grokptah.help.v1",
    sourceArticleId: entry.id,
    text: entry.body,
    sources: freezeSources(overlay.sources),
  };
}

/**
 * Fold a capability entry into the canonical article it duplicates.
 *
 * Retrieval signal (aliases, keywords, audience, capabilities) merges
 * permissively so either corpus's vocabulary finds the article; access merges
 * to the more restrictive of the two, so unification can never widen who is
 * shown a gated article.
 */
function mergeCapabilityEntry(draft: ArticleDraft, entry: HelpEntry): void {
  draft.passages.push(capabilityPassage(entry, draft.id));
  draft.aliases = unionStrings(draft.aliases, entry.tags, [entry.title]);
  draft.keywords = unionStrings(draft.keywords, entry.keywords);
  draft.audience = HELP_AUDIENCES.filter(
    (value) => draft.audience.includes(value) || entry.audience.includes(value),
  );
  draft.access = ACCESS_RANK[entry.access] > ACCESS_RANK[draft.access]
    ? entry.access
    : draft.access;
  draft.capabilityIds = [...new Set([...draft.capabilityIds, ...entry.capabilityIds])].sort();
  draft.provenance.push({ corpus: "grokptah.help.v1", sourceArticleId: entry.id });
}

function draftFromCapabilityEntry(entry: HelpEntry): ArticleDraft {
  const overlay = CAPABILITY_CORPUS_METADATA[entry.id];
  if (!overlay) {
    throw new Error(`help authority: capability entry ${entry.id} has no topic/source metadata`);
  }
  const id = `${CAPABILITY_ID_PREFIX}${entry.id}`;
  return {
    id,
    title: entry.title,
    topic: overlay.topic,
    summary: entry.summary,
    passages: [capabilityPassage(entry, id)],
    // The capability corpus calls its alias vocabulary "tags"; both are
    // alias-weighted retrieval signals under the canonical schema.
    aliases: [...entry.tags],
    keywords: [...entry.keywords],
    audience: [...entry.audience],
    access: entry.access,
    capabilityIds: [...entry.capabilityIds],
    provenance: [{ corpus: "grokptah.help.v1", sourceArticleId: entry.id }],
  };
}

function sealDraft(draft: ArticleDraft): HelpAuthorityArticle {
  const passages = [...draft.passages].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  return Object.freeze({
    id: draft.id,
    title: draft.title,
    topic: draft.topic,
    summary: draft.summary,
    passages: Object.freeze(passages.map((passage) => Object.freeze({
      ...passage,
      sources: freezeSources(passage.sources),
    }))),
    aliases: frozenStrings(draft.aliases),
    keywords: frozenStrings(draft.keywords),
    audience: Object.freeze([...draft.audience]),
    access: draft.access,
    capabilityIds: frozenStrings(draft.capabilityIds),
    sources: freezeSources(unionSources(...passages.map((passage) => passage.sources))),
    provenance: Object.freeze(draft.provenance.map((record) => Object.freeze({ ...record }))),
  });
}

function assembleCanonicalCorpus(): readonly HelpAuthorityArticle[] {
  const drafts = new Map<string, ArticleDraft>();
  for (const article of HELP_ARTICLES) {
    if (drafts.has(article.id)) {
      throw new Error(`help authority: duplicate product article ${article.id}`);
    }
    drafts.set(article.id, draftFromProductArticle(article));
  }
  for (const entry of HELP_ENTRIES) {
    const target = CAPABILITY_MERGE_TARGET[entry.id];
    if (target === undefined) {
      const draft = draftFromCapabilityEntry(entry);
      if (drafts.has(draft.id)) {
        throw new Error(`help authority: duplicate capability article ${draft.id}`);
      }
      drafts.set(draft.id, draft);
      continue;
    }
    const host = drafts.get(target);
    if (!host) {
      throw new Error(
        `help authority: capability entry ${entry.id} names unknown merge target ${target}`,
      );
    }
    mergeCapabilityEntry(host, entry);
  }

  const articles = [...drafts.values()]
    .map(sealDraft)
    .sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
  const validation = validateHelpAuthorityCorpus(articles);
  if (!validation.valid) {
    const detail = validation.issues
      .slice(0, 5)
      .map((issue) => `${issue.articleId}: ${issue.code} (${issue.detail})`)
      .join("; ");
    throw new Error(`help authority: canonical corpus failed validation — ${detail}`);
  }
  return Object.freeze(articles);
}

/**
 * The single canonical Help corpus.
 *
 * Assembly fails closed: a missing overlay, a duplicate ID, an unknown merge
 * target, an unsafe link, or an unknown schema field throws at module load
 * rather than shipping a partially-valid corpus.
 */
export const HELP_AUTHORITY_ARTICLES: readonly HelpAuthorityArticle[] = assembleCanonicalCorpus();

/** Every passage text of an article, joined for indexing and previews. */
export function helpArticleText(article: HelpAuthorityArticle): string {
  return article.passages.map((passage) => passage.text).join("\n\n");
}

/* ------------------------------------------------------------------ *
 * Digest and manifest
 * ------------------------------------------------------------------ */

const FNV_OFFSET_BASIS = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const U64 = 0xffffffffffffffffn;

/**
 * FNV-1a over UTF-8, rendered as 16 lowercase hex digits.
 *
 * This is a drift detector, not a cryptographic commitment: it proves a
 * corpus is byte-identical to the one a manifest was recorded against, and
 * nothing about who produced it. It is used instead of a Web Crypto digest
 * because the public bundle must stay synchronous and dependency-free.
 */
export function helpDigest(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let hash = FNV_OFFSET_BASIS;
  for (const byte of bytes) {
    hash = (hash ^ BigInt(byte)) & U64;
    hash = (hash * FNV_PRIME) & U64;
  }
  return hash.toString(16).padStart(16, "0");
}

function canonicalSourceJson(sources: readonly HelpSource[]): unknown {
  return sources.map((source) => [source.id, source.path, source.heading]);
}

/** Serialize an article with a fixed key order so the digest is stable. */
function canonicalArticleJson(article: HelpAuthorityArticle): string {
  return JSON.stringify([
    article.id,
    article.title,
    article.topic,
    article.summary,
    article.passages.map((passage) => [
      passage.id,
      passage.corpus,
      passage.sourceArticleId,
      passage.text,
      canonicalSourceJson(passage.sources),
    ]),
    article.aliases,
    article.keywords,
    article.audience,
    article.access,
    article.capabilityIds,
    canonicalSourceJson(article.sources),
    article.provenance.map((record) => [record.corpus, record.sourceArticleId]),
  ]);
}

/** Digest a corpus independently of JavaScript key-insertion order. */
export function digestHelpCorpus(
  articles: readonly HelpAuthorityArticle[] = HELP_AUTHORITY_ARTICLES,
): string {
  return helpDigest([
    HELP_AUTHORITY_CONTRACT,
    HELP_AUTHORITY_CORPUS_VERSION,
    ...articles.map(canonicalArticleJson),
  ].join("\n"));
}

export type HelpManifestSource = {
  readonly id: string;
  readonly path: string;
  readonly heading: string;
  readonly articleIds: readonly string[];
};

export type HelpManifestCorpus = {
  readonly id: HelpCorpusId;
  readonly version: string;
  readonly articleCount: number;
  readonly passageCount: number;
};

export type HelpManifest = {
  readonly contract: typeof HELP_AUTHORITY_CONTRACT;
  readonly corpusVersion: typeof HELP_AUTHORITY_CORPUS_VERSION;
  readonly digest: string;
  readonly digestAlgorithm: "fnv1a-64";
  readonly articleCount: number;
  readonly passageCount: number;
  readonly articleIds: readonly string[];
  readonly corpora: readonly HelpManifestCorpus[];
  readonly sources: readonly HelpManifestSource[];
  readonly capabilityIds: readonly string[];
  readonly retrievalMode: "offline-hybrid";
};

function buildManifest(
  articles: readonly HelpAuthorityArticle[] = HELP_AUTHORITY_ARTICLES,
): HelpManifest {
  const sources = new Map<string, { path: string; heading: string; articleIds: Set<string> }>();
  for (const article of articles) {
    for (const source of article.sources) {
      const key = `${source.id}::${source.path}::${source.heading}`;
      const existing = sources.get(key);
      if (existing) existing.articleIds.add(article.id);
      else {
        sources.set(key, {
          path: source.path,
          heading: source.heading,
          articleIds: new Set([article.id]),
        });
      }
    }
  }
  const corpora: HelpManifestCorpus[] = HELP_SOURCE_CORPORA.map((id) => ({
    id,
    version: id === "product-corpus-v1" ? HELP_CORPUS_VERSION : id,
    articleCount: articles.filter((article) =>
      article.provenance.some((record) => record.corpus === id)).length,
    passageCount: articles.reduce((total, article) =>
      total + article.passages.filter((passage) => passage.corpus === id).length, 0),
  }));
  return Object.freeze({
    contract: HELP_AUTHORITY_CONTRACT,
    corpusVersion: HELP_AUTHORITY_CORPUS_VERSION,
    digest: digestHelpCorpus(articles),
    digestAlgorithm: "fnv1a-64" as const,
    articleCount: articles.length,
    passageCount: articles.reduce((total, article) => total + article.passages.length, 0),
    articleIds: frozenStrings(articles.map((article) => article.id)),
    corpora: Object.freeze(corpora.map((corpus) => Object.freeze(corpus))),
    sources: Object.freeze(
      [...sources.entries()]
        .map(([key, value]) => Object.freeze({
          id: key.slice(0, key.indexOf("::")),
          path: value.path,
          heading: value.heading,
          articleIds: frozenStrings([...value.articleIds].sort()),
        }))
        .sort((a, b) => {
          const left = `${a.id}::${a.path}::${a.heading}`;
          const right = `${b.id}::${b.path}::${b.heading}`;
          return left < right ? -1 : left > right ? 1 : 0;
        }),
    ),
    capabilityIds: frozenStrings(
      [...new Set(articles.flatMap((article) => [...article.capabilityIds]))].sort(),
    ),
    retrievalMode: "offline-hybrid" as const,
  });
}

export const HELP_AUTHORITY_MANIFEST: HelpManifest = buildManifest();

/**
 * The digest recorded for this corpus version.
 *
 * Changing any article text, metadata, source, passage, or provenance changes
 * the computed digest. `verifyHelpAuthorityManifest` then reports drift, and
 * the headless API refuses to serve a corpus that does not match.
 */
export const HELP_AUTHORITY_DIGEST = "e1ab7f80506afceb" as const;

export type HelpManifestVerification = {
  readonly ok: boolean;
  readonly expected: string;
  readonly actual: string;
  readonly reason: "verified" | "digest-mismatch" | "corpus-invalid";
  readonly issues: readonly HelpCorpusIssue[];
};

/** Recompute the digest and report drift against the recorded manifest. */
export function verifyHelpAuthorityManifest(
  articles: readonly HelpAuthorityArticle[] = HELP_AUTHORITY_ARTICLES,
  expected: string = HELP_AUTHORITY_DIGEST,
): HelpManifestVerification {
  const validation = validateHelpAuthorityCorpus(articles);
  const actual = digestHelpCorpus(articles);
  if (!validation.valid) {
    return { ok: false, expected, actual, reason: "corpus-invalid", issues: validation.issues };
  }
  if (actual !== expected) {
    return { ok: false, expected, actual, reason: "digest-mismatch", issues: [] };
  }
  return { ok: true, expected, actual, reason: "verified", issues: [] };
}

/* ------------------------------------------------------------------ *
 * Tokenisation and the hybrid index
 * ------------------------------------------------------------------ */

const HELP_STOP_WORDS: ReadonlySet<string> = new Set([
  "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for",
  "from", "has", "have", "how", "i", "in", "is", "it", "me", "my", "no", "not",
  "of", "on", "or", "so", "that", "the", "this", "to", "was", "what", "when",
  "why", "will", "with", "you", "your",
]);

/**
 * Fold a raw word to its canonical retrieval term.
 *
 * Deliberately a small, explicit normaliser rather than a general stemmer:
 * every transformation here is reviewable, and identical input always yields
 * identical output, which is what makes the ranking reproducible.
 */
export function canonicalHelpTerm(value: string): string {
  const normalized = value.normalize("NFKD").replace(/\p{M}/gu, "").toLocaleLowerCase("en-US");
  if (normalized.length > 4 && normalized.endsWith("ies")) return `${normalized.slice(0, -3)}y`;
  if (
    normalized.length > 4 &&
    normalized.endsWith("s") &&
    !normalized.endsWith("ss") &&
    !normalized.endsWith("us")
  ) {
    return normalized.slice(0, -1);
  }
  return normalized;
}

/** Split text into canonical, stop-word-free retrieval terms. */
export function helpTerms(value: string): string[] {
  return value
    .split(/[^\p{L}\p{N}]+/u)
    .map(canonicalHelpTerm)
    .filter((term) => term.length > 1 && !HELP_STOP_WORDS.has(term));
}

export type HelpSearchField = "title" | "keywords" | "aliases" | "summary" | "body";

/** Token weights by field. Identifier-bearing fields outrank prose. */
const TOKEN_FIELD_WEIGHT: Readonly<Record<HelpSearchField, number>> = Object.freeze({
  title: 12, keywords: 9, aliases: 7, summary: 5, body: 2,
});

/** Phrase weights for the lexical pass, which matches raw contiguous text. */
const PHRASE_FIELD_WEIGHT: Readonly<Record<HelpSearchField, number>> = Object.freeze({
  title: 30, keywords: 14, aliases: 12, summary: 10, body: 4,
});

export type HelpIndexedArticle = {
  readonly article: HelpAuthorityArticle;
  readonly tokens: Readonly<Record<HelpSearchField, readonly string[]>>;
  readonly lexical: Readonly<Record<HelpSearchField, string>>;
};

function lowerJoin(values: readonly string[]): string {
  return values.join(" ").toLocaleLowerCase("en-US");
}

/** Build the deterministic hybrid index for a corpus. */
export function buildHelpAuthorityIndex(
  articles: readonly HelpAuthorityArticle[] = HELP_AUTHORITY_ARTICLES,
): readonly HelpIndexedArticle[] {
  return Object.freeze(articles.map((article) => {
    const text = helpArticleText(article);
    return Object.freeze({
      article,
      tokens: Object.freeze({
        title: frozenStrings(helpTerms(article.title)),
        keywords: frozenStrings(article.keywords.flatMap(helpTerms)),
        aliases: frozenStrings(article.aliases.flatMap(helpTerms)),
        summary: frozenStrings(helpTerms(article.summary)),
        body: frozenStrings(helpTerms(text)),
      }),
      lexical: Object.freeze({
        title: article.title.toLocaleLowerCase("en-US"),
        keywords: lowerJoin(article.keywords),
        aliases: lowerJoin(article.aliases),
        summary: article.summary.toLocaleLowerCase("en-US"),
        body: text.toLocaleLowerCase("en-US"),
      }),
    });
  }));
}

export const HELP_AUTHORITY_INDEX = buildHelpAuthorityIndex();

/**
 * Inverse document frequency over the indexed corpus.
 *
 * A term present in every article carries almost no ranking signal; a term
 * present in one carries the most. Computed from the frozen corpus, so it is
 * a constant of the contract rather than a per-query heuristic.
 */
function buildIdf(index: readonly HelpIndexedArticle[]): ReadonlyMap<string, number> {
  const documentFrequency = new Map<string, number>();
  for (const entry of index) {
    const present = new Set<string>();
    for (const field of Object.keys(entry.tokens) as HelpSearchField[]) {
      for (const term of entry.tokens[field]) present.add(term);
    }
    for (const term of present) {
      documentFrequency.set(term, (documentFrequency.get(term) ?? 0) + 1);
    }
  }
  const total = index.length;
  const idf = new Map<string, number>();
  for (const [term, frequency] of documentFrequency) {
    idf.set(term, Math.log(1 + total / (1 + frequency)));
  }
  return Object.freeze(idf);
}

/** IDF for an unseen term: treated as maximally rare but never infinite. */
function idfFor(idf: ReadonlyMap<string, number>, term: string, total: number): number {
  return idf.get(term) ?? Math.log(1 + total);
}

/** IDF over the shipped corpus, computed once. */
const HELP_AUTHORITY_IDF = buildIdf(HELP_AUTHORITY_INDEX);

/* ------------------------------------------------------------------ *
 * Retrieval contract
 * ------------------------------------------------------------------ */

export type HelpSignalKind = "token" | "phrase";

export type HelpSignal = {
  readonly kind: HelpSignalKind;
  readonly field: HelpSearchField;
  readonly term: string;
  /** Contribution of this signal to the fused score. */
  readonly weight: number;
};

export type HelpExplanation = {
  readonly tokenScore: number;
  readonly lexicalScore: number;
  readonly score: number;
  readonly confidence: number;
  /** Fraction of query terms this article matched, in [0, 1]. */
  readonly coverage: number;
  readonly signals: readonly HelpSignal[];
};

/** Where a quoted span lives. Passage spans carry their own sources. */
export type HelpSpanField = "title" | "summary" | "passage";

export type HelpCitationSpan = {
  readonly articleId: string;
  readonly field: HelpSpanField;
  /** Set when field is "passage", otherwise null. */
  readonly passageId: string | null;
  /** UTF-16 offsets into the named field's text. */
  readonly start: number;
  readonly end: number;
  /** Exact substring of that text: text.slice(start, end). */
  readonly quote: string;
  readonly term: string;
  /** The documents backing this exact span. */
  readonly sources: readonly HelpSource[];
};

export type HelpCitation = {
  readonly articleId: string;
  readonly sources: readonly HelpSource[];
  readonly spans: readonly HelpCitationSpan[];
};

export type HelpHit = {
  readonly article: HelpAuthorityArticle;
  readonly score: number;
  readonly confidence: number;
  readonly matchedTerms: readonly string[];
  readonly explanation: HelpExplanation;
  readonly citation: HelpCitation;
};

export type HelpRetrievalOutcome = "answer" | "abstain" | "rejected";

export type HelpAbstainReason =
  | "empty-query"
  | "no-match"
  | "low-confidence"
  | "ambiguous";

export type HelpQueryRejection =
  | "not-a-string"
  | "query-too-long"
  | "query-too-many-bytes"
  | "control-characters"
  | "invalid-limit"
  | "invalid-audience"
  | "invalid-topic";

export type HelpSearchRequest = {
  readonly limit?: number;
  readonly topic?: HelpTopic | "all";
  readonly audience?: HelpAuthorityAudience;
  readonly capabilityIds?: readonly string[];
  /** Gated and operator articles stay out of results unless asked for. */
  readonly includeRestricted?: boolean;
};

export type HelpRetrievalResult = {
  readonly contract: typeof HELP_AUTHORITY_CONTRACT;
  readonly corpusVersion: typeof HELP_AUTHORITY_CORPUS_VERSION;
  readonly digest: string;
  readonly retrievalMode: "offline-hybrid";
  readonly outcome: HelpRetrievalOutcome;
  readonly abstainReason: HelpAbstainReason | null;
  readonly rejection: HelpQueryRejection | null;
  /** The query as searched, after bounds are applied. */
  readonly query: string;
  readonly queryTerms: readonly string[];
  /**
   * Ranked candidates. Present even when abstaining, so a caller can offer
   * them as suggestions — but an abstained result is never an answer.
   */
  readonly hits: readonly HelpHit[];
  readonly totalMatched: number;
  readonly limit: number;
};

function round6(value: number): number {
  return Math.round(value * 1e6) / 1e6;
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

function utf8Length(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function rejected(
  rejection: HelpQueryRejection,
  digest: string,
  limit: number,
): HelpRetrievalResult {
  return Object.freeze({
    contract: HELP_AUTHORITY_CONTRACT,
    corpusVersion: HELP_AUTHORITY_CORPUS_VERSION,
    digest,
    retrievalMode: "offline-hybrid" as const,
    outcome: "rejected" as const,
    abstainReason: null,
    rejection,
    query: "",
    queryTerms: Object.freeze([]),
    hits: Object.freeze([]),
    totalMatched: 0,
    limit,
  });
}

/** Contiguous phrases of the raw query: the whole query, then its bigrams. */
function queryPhrases(query: string): string[] {
  const words = query
    .toLocaleLowerCase("en-US")
    .split(/[^\p{L}\p{N}]+/u)
    .filter((word) => word.length > 0);
  const phrases: string[] = [];
  if (words.length > 1) phrases.push(words.join(" "));
  for (let index = 0; index + 1 < words.length; index += 1) {
    phrases.push(`${words[index]} ${words[index + 1]}`);
  }
  return [...new Set(phrases)];
}

const WORD_CHARACTER = /[\p{L}\p{N}]/u;

/** Expand a match to whole words so a quote reads as text, not a fragment. */
function spanAt(text: string, at: number, length: number): { start: number; end: number } {
  let start = at;
  while (start > 0 && WORD_CHARACTER.test(text[start - 1] as string)) start -= 1;
  let end = at + length;
  while (end < text.length && WORD_CHARACTER.test(text[end] as string)) end += 1;
  if (end - start > HELP_MAX_SPAN_CHARS) end = start + HELP_MAX_SPAN_CHARS;
  return { start, end };
}

/**
 * Locate bounded, exact spans supporting the match.
 *
 * Every span records the field, offsets, quoted substring, and the documents
 * behind that specific text, so a consumer can show a citation the reader can
 * check rather than a bare article reference.
 */
function citationSpans(
  article: HelpAuthorityArticle,
  matchedTerms: readonly string[],
): HelpCitationSpan[] {
  const spans: HelpCitationSpan[] = [];
  const push = (
    field: HelpSpanField,
    passageId: string | null,
    text: string,
    term: string,
    sources: readonly HelpSource[],
  ): boolean => {
    if (spans.length >= HELP_MAX_SPANS_PER_HIT) return false;
    const at = text.toLocaleLowerCase("en-US").indexOf(term);
    if (at < 0) return true;
    const { start, end } = spanAt(text, at, term.length);
    spans.push(Object.freeze({
      articleId: article.id,
      field,
      passageId,
      start,
      end,
      quote: text.slice(start, end),
      term,
      sources,
    }));
    return true;
  };

  for (const term of matchedTerms) {
    if (!push("title", null, article.title, term, article.sources)) return spans;
  }
  for (const term of matchedTerms) {
    if (!push("summary", null, article.summary, term, article.sources)) return spans;
  }
  for (const passage of article.passages) {
    for (const term of matchedTerms) {
      if (!push("passage", passage.id, passage.text, term, passage.sources)) return spans;
    }
  }
  return spans;
}

type ScoredCandidate = {
  entry: HelpIndexedArticle;
  tokenScore: number;
  lexicalScore: number;
  score: number;
  matchedTerms: string[];
  signals: HelpSignal[];
};

const FIELD_ORDER: readonly HelpSearchField[] = Object.freeze([
  "title", "keywords", "aliases", "summary", "body",
]);

function scoreCandidate(
  entry: HelpIndexedArticle,
  queryTerms: readonly string[],
  phrases: readonly string[],
  idf: ReadonlyMap<string, number>,
  corpusSize: number,
): ScoredCandidate | null {
  const signals: HelpSignal[] = [];
  const matchedTerms: string[] = [];
  let tokenScore = 0;

  // Token pass: each query term scores once, at its best-weighted field.
  for (const term of queryTerms) {
    let bestField: HelpSearchField | null = null;
    for (const field of FIELD_ORDER) {
      if (!entry.tokens[field].includes(term)) continue;
      if (bestField === null || TOKEN_FIELD_WEIGHT[field] > TOKEN_FIELD_WEIGHT[bestField]) {
        bestField = field;
      }
    }
    if (bestField === null) continue;
    const weight = round6(TOKEN_FIELD_WEIGHT[bestField] * idfFor(idf, term, corpusSize));
    tokenScore += weight;
    matchedTerms.push(term);
    signals.push({ kind: "token", field: bestField, term, weight });
  }

  // Lexical pass: contiguous phrases matched against raw field text. This is
  // what lets "restricted company gateway" beat an article that merely
  // mentions each of those words in unrelated sentences.
  let lexicalScore = 0;
  for (const phrase of phrases) {
    for (const field of FIELD_ORDER) {
      if (!entry.lexical[field].includes(phrase)) continue;
      const words = phrase.split(" ").length;
      const weight = round6(PHRASE_FIELD_WEIGHT[field] * (words > 2 ? 1 : 0.5));
      lexicalScore += weight;
      signals.push({ kind: "phrase", field, term: phrase, weight });
    }
  }

  if (matchedTerms.length === 0 && lexicalScore === 0) return null;
  return {
    entry,
    tokenScore: round6(tokenScore),
    lexicalScore: round6(lexicalScore),
    score: round6(tokenScore + lexicalScore),
    matchedTerms,
    signals,
  };
}

/** Rank signals deterministically for a stable, readable explanation. */
function orderedSignals(signals: readonly HelpSignal[]): readonly HelpSignal[] {
  return Object.freeze(
    [...signals]
      .sort((a, b) =>
        b.weight - a.weight ||
        FIELD_ORDER.indexOf(a.field) - FIELD_ORDER.indexOf(b.field) ||
        (a.term < b.term ? -1 : a.term > b.term ? 1 : 0))
      .slice(0, HELP_MAX_EXPLANATION_SIGNALS)
      .map((signal) => Object.freeze(signal)),
  );
}

function passesFilters(article: HelpAuthorityArticle, request: HelpSearchRequest): boolean {
  if (request.topic && request.topic !== "all" && article.topic !== request.topic) return false;
  if (!request.includeRestricted && article.access !== "public") return false;
  if (request.audience && !article.audience.includes(request.audience)) return false;
  const wanted = request.capabilityIds ?? [];
  if (wanted.length > 0 && !article.capabilityIds.some((id) => wanted.includes(id))) return false;
  return true;
}

/**
 * Offline hybrid retrieval over the canonical corpus.
 *
 * Fails closed on a malformed or oversized query and abstains rather than
 * guessing when the leader is weak or a runner-up is indistinguishable from
 * it. Ranking is a pure function of (corpus, query, request): no clock, no
 * randomness, and no locale-sensitive comparison in the tie-break.
 */
export function searchHelpAuthority(
  query: string,
  request: HelpSearchRequest = {},
  index: readonly HelpIndexedArticle[] = HELP_AUTHORITY_INDEX,
): HelpRetrievalResult {
  const digest = HELP_AUTHORITY_MANIFEST.digest;
  const rawLimit = request.limit ?? HELP_DEFAULT_RESULTS;
  if (!Number.isInteger(rawLimit) || rawLimit < 1 || rawLimit > HELP_MAX_RESULTS) {
    return rejected("invalid-limit", digest, HELP_DEFAULT_RESULTS);
  }
  const limit = rawLimit;
  if (typeof query !== "string") return rejected("not-a-string", digest, limit);
  if (query.length > HELP_MAX_QUERY_CHARS) return rejected("query-too-long", digest, limit);
  if (utf8Length(query) > HELP_MAX_QUERY_BYTES) {
    return rejected("query-too-many-bytes", digest, limit);
  }
  if (hasControlCharacters(query)) return rejected("control-characters", digest, limit);
  if (request.audience !== undefined && !HELP_AUDIENCES.includes(request.audience)) {
    return rejected("invalid-audience", digest, limit);
  }
  if (request.topic !== undefined && request.topic !== "all" &&
      !HELP_TOPICS.includes(request.topic)) {
    return rejected("invalid-topic", digest, limit);
  }

  const trimmed = query.trim();
  const queryTerms = [...new Set(helpTerms(trimmed))];
  const base = {
    contract: HELP_AUTHORITY_CONTRACT,
    corpusVersion: HELP_AUTHORITY_CORPUS_VERSION,
    digest,
    retrievalMode: "offline-hybrid" as const,
    rejection: null,
    query: trimmed,
    queryTerms: frozenStrings(queryTerms),
    limit,
  };
  if (queryTerms.length === 0) {
    return Object.freeze({
      ...base,
      outcome: "abstain" as const,
      abstainReason: "empty-query" as const,
      hits: Object.freeze([]),
      totalMatched: 0,
    });
  }

  const idf = index === HELP_AUTHORITY_INDEX ? HELP_AUTHORITY_IDF : buildIdf(index);
  const phrases = queryPhrases(trimmed);
  const idealScore = queryTerms.reduce(
    (total, term) => total + TOKEN_FIELD_WEIGHT.title * idfFor(idf, term, index.length),
    0,
  );

  const scored = index
    .filter((entry) => passesFilters(entry.article, request))
    .map((entry) => scoreCandidate(entry, queryTerms, phrases, idf, index.length))
    .filter((candidate): candidate is ScoredCandidate => candidate !== null)
    // Deterministic order: score desc, then canonical ID ascending. The
    // tie-break compares code points rather than using localeCompare, so the
    // ranking does not shift with the host locale.
    .sort((a, b) =>
      b.score - a.score ||
      (a.entry.article.id < b.entry.article.id ? -1
        : a.entry.article.id > b.entry.article.id ? 1 : 0));

  const hits: HelpHit[] = scored.slice(0, limit).map((candidate) => {
    const confidence = idealScore === 0
      ? 0
      : round6(clamp(candidate.score / idealScore, 0, 0.99));
    return Object.freeze({
      article: candidate.entry.article,
      score: candidate.score,
      confidence,
      matchedTerms: frozenStrings(candidate.matchedTerms),
      explanation: Object.freeze({
        tokenScore: candidate.tokenScore,
        lexicalScore: candidate.lexicalScore,
        score: candidate.score,
        confidence,
        coverage: round6(candidate.matchedTerms.length / queryTerms.length),
        signals: orderedSignals(candidate.signals),
      }),
      citation: Object.freeze({
        articleId: candidate.entry.article.id,
        sources: candidate.entry.article.sources,
        spans: Object.freeze(citationSpans(candidate.entry.article, candidate.matchedTerms)),
      }),
    });
  });

  let abstainReason: HelpAbstainReason | null = null;
  const top = hits[0];
  const runnerUp = hits[1];
  if (!top) {
    abstainReason = "no-match";
  } else if (top.confidence < HELP_MIN_CONFIDENCE) {
    abstainReason = "low-confidence";
  } else if (
    runnerUp &&
    top.confidence < HELP_CLEAR_CONFIDENCE &&
    top.score > 0 &&
    runnerUp.score / top.score >= HELP_AMBIGUITY_RATIO
  ) {
    abstainReason = "ambiguous";
  }

  return Object.freeze({
    ...base,
    outcome: abstainReason === null ? ("answer" as const) : ("abstain" as const),
    abstainReason,
    hits: Object.freeze(hits),
    totalMatched: scored.length,
  });
}

/* ------------------------------------------------------------------ *
 * Headless retrieval API
 * ------------------------------------------------------------------ */

export type HelpAuthorityOptions = {
  /** Serve a caller-supplied corpus instead of the shipped one. */
  readonly articles?: readonly HelpAuthorityArticle[];
  /** Digest the corpus must match. Defaults to the recorded manifest digest. */
  readonly expectedDigest?: string;
  /** Set false only to inspect a drifted corpus; verify() still reports it. */
  readonly requireVerifiedDigest?: boolean;
};

/**
 * A ready-to-use Help retrieval surface for an external consumer.
 *
 * Everything here is synchronous, pure, and free of transport, credentials,
 * and native bindings, so a browser product can hold one instance for the
 * lifetime of a page. It is the only entry point an embedder needs.
 */
export type HelpAuthority = {
  readonly contract: typeof HELP_AUTHORITY_CONTRACT;
  readonly manifest: HelpManifest;
  readonly articles: readonly HelpAuthorityArticle[];
  /** Offline hybrid retrieval with explanations, citations, and abstention. */
  search(query: string, request?: HelpSearchRequest): HelpRetrievalResult;
  /** Look up one canonical article by its stable ID. */
  article(id: string): HelpAuthorityArticle | null;
  /** Re-derive a citation quote from the corpus to prove the span is exact. */
  resolveSpan(span: HelpCitationSpan): string | null;
  /** Recompute the digest and report drift against the recorded manifest. */
  verify(): HelpManifestVerification;
};

/**
 * Build a Help authority over a verified corpus.
 *
 * Fails closed: an invalid corpus, or a digest that does not match the
 * expected manifest, throws instead of serving results — a consumer that
 * cites a drifted corpus is citing something it cannot name.
 */
export function createHelpAuthority(options: HelpAuthorityOptions = {}): HelpAuthority {
  const articles = options.articles ?? HELP_AUTHORITY_ARTICLES;
  const expectedDigest = options.expectedDigest ?? (
    options.articles ? digestHelpCorpus(articles) : HELP_AUTHORITY_DIGEST
  );
  const requireVerified = options.requireVerifiedDigest ?? true;
  const verification = verifyHelpAuthorityManifest(articles, expectedDigest);
  if (requireVerified && !verification.ok) {
    const detail = verification.reason === "digest-mismatch"
      ? `expected ${verification.expected}, computed ${verification.actual}`
      : verification.issues
          .slice(0, 3)
          .map((issue) => `${issue.articleId}: ${issue.code}`)
          .join("; ");
    throw new Error(`help authority: refusing to serve corpus (${verification.reason}) — ${detail}`);
  }

  const shipped = articles === HELP_AUTHORITY_ARTICLES;
  const index = shipped ? HELP_AUTHORITY_INDEX : buildHelpAuthorityIndex(articles);
  const manifest = shipped ? HELP_AUTHORITY_MANIFEST : buildManifest(articles);
  const byId = new Map(articles.map((article) => [article.id, article]));

  return Object.freeze({
    contract: HELP_AUTHORITY_CONTRACT,
    manifest,
    articles,
    search: (query: string, request: HelpSearchRequest = {}) =>
      searchHelpAuthority(query, request, index),
    article: (id: string) => byId.get(id) ?? null,
    resolveSpan: (span: HelpCitationSpan) => {
      const article = byId.get(span.articleId);
      if (!article) return null;
      let text: string | null = null;
      if (span.field === "title") text = article.title;
      else if (span.field === "summary") text = article.summary;
      else text = article.passages.find((passage) => passage.id === span.passageId)?.text ?? null;
      if (text === null) return null;
      if (span.start < 0 || span.end > text.length || span.start >= span.end) return null;
      return text.slice(span.start, span.end);
    },
    verify: () => verifyHelpAuthorityManifest(articles, expectedDigest),
  });
}
