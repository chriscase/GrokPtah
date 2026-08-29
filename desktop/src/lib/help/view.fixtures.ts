/**
 * Deterministic synthetic fixtures for the Help surface.
 *
 * The donor tests search the *shipped* corpus and answer "does the real
 * documentation rank the right article". These answer a different question —
 * "can a surface reach and render every state the view contract defines" — and
 * so they deliberately do not use the shipped corpus at all.
 *
 * The reason is drift. A UI test written against real articles fails for two
 * unrelated causes: the component changed, or the documentation did. One
 * edited sentence can move a fused score across the abstention threshold and
 * turn an `answer` test red without a line of UI having changed. This is a
 * small fictional corpus instead, built so each outcome is reachable *by
 * construction*:
 *
 *   - `answer`         one article owns the query's words outright
 *   - `ambiguous`      two articles are word-for-word symmetric on the query
 *   - `low-confidence` something matches, nothing well enough to lead
 *   - `no-match`       the query's words appear nowhere in the corpus
 *   - `rejected`       the query fails a bound before retrieval runs
 *   - `browse`         there is no query yet
 *
 * The articles describe an invented product ("Lantern") so no fixture can be
 * mistaken for GrokPtah guidance. Nothing here is a measurement: no clock, no
 * randomness, no network, no model output.
 *
 * Digests are placeholders. The view path resolves records by id and compares
 * chunk bytes; it never verifies a corpus digest, and `verifyHelpCorpus` — the
 * function that does — is exercised against the real corpora by
 * `helpCorpus.test.ts`. A fixture carrying invented digests would be checked
 * by nothing and would only invite a reader to trust them.
 */

import type { HelpCorpus } from "./generated/contract";

const SOURCE = (id: string, path: string, heading: string) => ({
  id,
  path,
  heading,
  visibility: "public" as const,
  digest: `fixture-source-${id}`,
});

const ARTICLE = (
  id: string,
  title: string,
  topic: "getting-started" | "providers" | "computer-use" | "operations",
  summary: string,
  body: string,
  sourceIds: readonly string[],
) => ({
  id,
  title,
  topic,
  summary,
  body,
  aliases: [] as readonly string[],
  keywords: [] as readonly string[],
  source_ids: sourceIds,
  visibility: "public" as const,
  capability_ids: [] as readonly string[],
  digest: `fixture-article-${id}`,
});

const CHUNK = (articleId: string, ordinal: number, text: string, sourceIds: readonly string[]) => ({
  id: `${articleId}#body.${ordinal}`,
  article_id: articleId,
  kind: "body" as const,
  ordinal,
  text,
  locale: "en",
  source_ids: sourceIds,
  visibility: "public" as const,
  digest: `fixture-chunk-${articleId}-${ordinal}`,
});

const GUIDE = SOURCE("fixture.lantern-guide", "docs/synthetic/lantern-guide.md", "Lantern workspaces");
const RUNBOOK = SOURCE("fixture.lantern-runbook", "docs/synthetic/lantern-runbook.md", "Relay rotation");

/**
 * A four-article fictional corpus.
 *
 * Word placement is the whole design. "Lantern workspace pane" belongs to one
 * article and nothing else, so it leads decisively. The two relay articles are
 * word-for-word symmetric apart from their compass direction, so a query for
 * what they share cannot prefer one — which is the shape the view calls
 * ambiguous. "Cartography" appears once, in prose, so a query pairing it with
 * a word the corpus has never seen matches weakly and leads nothing. Move any
 * of these words and the fixture stops testing what it claims to.
 */
export const HELP_VIEW_FIXTURE_CORPUS: HelpCorpus = {
  schema_version: "grokptah-help.v1",
  content_version: "fixture-v1",
  sources: [GUIDE, RUNBOOK],
  articles: [
    ARTICLE(
      "fixture.lantern-workspace",
      "Set up the Lantern workspace",
      "getting-started",
      "Create a Lantern workspace and keep its panes independent.",
      "A Lantern workspace holds one pane per task.",
      [GUIDE.id],
    ),
    ARTICLE(
      "fixture.northern-relay",
      "Northern relay rotation",
      "operations",
      "Rotate the northern relay without dropping in-flight work.",
      "Rotate the northern relay during a quiet window.",
      [RUNBOOK.id],
    ),
    ARTICLE(
      "fixture.southern-relay",
      "Southern relay rotation",
      "operations",
      "Rotate the southern relay without dropping in-flight work.",
      "Rotate the southern relay during a quiet window.",
      [RUNBOOK.id],
    ),
    ARTICLE(
      "fixture.cartography",
      "Cartography panes",
      "operations",
      "Cartography panes redraw on their own schedule.",
      "Cartography panes redraw on their own schedule.",
      [GUIDE.id],
    ),
  ],
  chunks: [
    CHUNK(
      "fixture.lantern-workspace",
      0,
      "A Lantern workspace holds one pane per task. Panes do not share state, so a long task cannot stall a short one.",
      [GUIDE.id],
    ),
    CHUNK(
      "fixture.northern-relay",
      0,
      "Rotate the northern relay during a quiet window. The relay keeps its last acknowledged position, so a rotation does not restart in-flight work.",
      [RUNBOOK.id],
    ),
    CHUNK(
      "fixture.southern-relay",
      0,
      "Rotate the southern relay during a quiet window. The relay keeps its last acknowledged position, so a rotation does not restart in-flight work.",
      [RUNBOOK.id],
    ),
    CHUNK(
      "fixture.cartography",
      0,
      "Cartography panes redraw on their own schedule and are documented separately.",
      [GUIDE.id],
    ),
  ],
  digest: "fixture-corpus-digest",
  source_digest: "fixture-source-digest",
};

/** A corpus with no chunks at all, for the empty-corpus path. */
export const HELP_VIEW_EMPTY_CORPUS: HelpCorpus = {
  ...HELP_VIEW_FIXTURE_CORPUS,
  articles: [],
  chunks: [],
};

/**
 * A corpus whose chunk bytes disagree with what retrieval reported.
 *
 * Built by mutating a chunk *after* the copy retrieval will describe, so the
 * view's re-read finds different bytes and must drop the result rather than
 * quote text the corpus does not contain.
 */
export function corpusWithTamperedChunk(chunkId: string): HelpCorpus {
  return {
    ...HELP_VIEW_FIXTURE_CORPUS,
    chunks: HELP_VIEW_FIXTURE_CORPUS.chunks.map((chunk) =>
      chunk.id === chunkId ? { ...chunk, text: `${chunk.text} (altered after retrieval)` } : chunk,
    ),
  };
}

/** A corpus that names a source id it does not carry. */
export const HELP_VIEW_MISSING_SOURCE_CORPUS: HelpCorpus = {
  ...HELP_VIEW_FIXTURE_CORPUS,
  sources: HELP_VIEW_FIXTURE_CORPUS.sources.filter((source) => source.id !== RUNBOOK.id),
};

/** Queries that reach each state, named so a test reads as a table. */
export const HELP_VIEW_FIXTURE_QUERIES = Object.freeze({
  answer: "Lantern workspace pane",
  ambiguous: "rotate the relay during a quiet window",
  lowConfidence: "cartography zzzz",
  noMatch: "qqqq zzzz",
  browse: "",
});
