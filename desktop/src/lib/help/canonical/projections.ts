/**
 * Projections of the canonical corpus onto the two shipped Help contracts.
 *
 * `desktop/src/lib/help.ts` (`grokptah.help.v1`) and
 * `desktop/src/lib/helpCenter.ts` (`product-corpus-v1`) keep their published
 * shapes and their published behavior, but their data is generated here. There
 * is exactly one hand-maintained corpus in the tree (`data.ts`).
 */
import { HELP_CORPUS } from "./corpus";
import type { HelpAccess, HelpAudience, HelpCanonicalArticle, HelpSourceAnchor, HelpTopic } from "./types";

/** Legacy `grokptah.help.v1` entry shape. */
export type ProjectedHelpEntry = {
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

/** Legacy `product-corpus-v1` article shape. */
export type ProjectedHelpArticle = {
  readonly id: string;
  readonly title: string;
  readonly topic: HelpTopic;
  readonly summary: string;
  readonly body: string;
  readonly aliases: readonly string[];
  readonly keywords: readonly string[];
  readonly sources: readonly HelpSourceAnchor[];
};

/**
 * Entry projection.
 *
 * `tags` carries the canonical short keyword tokens and `keywords` carries the
 * natural-language aliases, matching how the two fields were weighted by the
 * original entry scorer (tags above keywords, both above prose).
 */
function projectEntry(article: HelpCanonicalArticle): ProjectedHelpEntry {
  return Object.freeze({
    id: article.legacyEntryId ?? article.id,
    title: article.title,
    summary: article.summary,
    body: article.body,
    tags: Object.freeze([...article.keywords]),
    keywords: Object.freeze([...article.aliases]),
    audience: Object.freeze([...article.audience]),
    access: article.access,
    capabilityIds: Object.freeze([...article.capabilityIds]),
  });
}

function projectArticle(article: HelpCanonicalArticle): ProjectedHelpArticle {
  return Object.freeze({
    id: article.id,
    title: article.title,
    topic: article.topic,
    summary: article.summary,
    body: article.body,
    aliases: Object.freeze([...article.aliases]),
    keywords: Object.freeze([...article.keywords]),
    sources: Object.freeze(article.sources.map((source) => Object.freeze({ ...source }))),
  });
}

/**
 * Generated `grokptah.help.v1` corpus.
 *
 * Scoped to the articles that carry a `legacyEntryId`. That contract shipped a
 * fixed, capability-indexed entry set, and widening it as a side effect of the
 * consolidation would silently change published ranking for existing consumers
 * — the not-yet-qualified isolated-guest article outranks the Computer Use
 * safety article under the legacy substring scorer. New content reaches
 * consumers through the canonical corpus and the hybrid retriever instead.
 */
export const PROJECTED_HELP_ENTRIES: readonly ProjectedHelpEntry[] = Object.freeze(
  HELP_CORPUS.articles.filter((article) => article.legacyEntryId).map(projectEntry),
);

/** Generated `product-corpus-v1` corpus. */
export const PROJECTED_HELP_ARTICLES: readonly ProjectedHelpArticle[] = Object.freeze(
  HELP_CORPUS.articles.map(projectArticle),
);

/** Legacy entry id -> canonical article id, for cross-contract citation. */
export const HELP_LEGACY_ENTRY_TO_ARTICLE: Readonly<Record<string, string>> = Object.freeze(
  Object.fromEntries(
    HELP_CORPUS.articles
      .filter((article) => article.legacyEntryId)
      .map((article) => [article.legacyEntryId as string, article.id]),
  ),
);
