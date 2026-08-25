/**
 * Canonical Help corpus schema.
 *
 * This is the single source of truth for Help content. The legacy
 * `grokptah.help.v1` entry corpus and the `product-corpus-v1` article corpus
 * are now *projections* generated from this data, so there is exactly one
 * hand-maintained corpus in the tree.
 *
 * Every article must carry at least one source anchor that resolves to a real
 * repository path and heading; `scripts/verify-help-corpus.mjs` fails closed
 * when an anchor drifts.
 */

/** Bumped when the shape of a canonical record changes. */
export const HELP_CANONICAL_SCHEMA_VERSION = "grokptah.help-canonical.v1" as const;
/** Bumped when the *content* changes in a way consumers should notice. */
export const HELP_CANONICAL_CONTENT_VERSION = "help-canonical-2026.08.1" as const;

export type HelpTopic = "getting-started" | "providers" | "computer-use" | "operations";
export type HelpAudience = "everyone" | "power_user" | "operator";
export type HelpAccess = "public" | "gated" | "operator";

/** The authored form of a citation target, before its digest is computed. */
export type HelpSourceSeed = {
  /** Stable citation id used in answers, e.g. `provider.profiles`. */
  readonly id: string;
  /** Repository-relative path. Must exist in the tree. */
  readonly path: string;
  /** Exact Markdown heading text within `path`. */
  readonly heading: string;
  /** Who may see content from this source. Absent means `public`. */
  readonly visibility?: HelpSourceVisibility;
};

/**
 * Who a source may be surfaced to.
 *
 * `project` and `private` are default-deny: they require an explicit,
 * action-time grant naming the same tenant and project.
 */
export type HelpSourceVisibility = "public" | "project" | "private";

/** A citation target plus its own domain-separated digest. */
export type HelpSourceAnchor = HelpSourceSeed & {
  readonly visibility: HelpSourceVisibility;
  /** `domainDigest(source, [id, path, heading, visibility])`. */
  readonly digest: string;
};

/** Locale-tagged surface text so supported articles are retrievable in-language. */
export type HelpLocalization = {
  readonly locale: string;
  readonly title: string;
  readonly summary: string;
  readonly keywords: readonly string[];
};

/**
 * A retrievable unit. Chunk IDs are stable across rebuilds because they are
 * derived from the article id, the chunk kind, and a stable ordinal.
 */
export type HelpChunk = {
  /** `${articleId}#${kind}.${ordinal}` — stable and citable. */
  readonly id: string;
  readonly articleId: string;
  readonly kind: "title" | "summary" | "body";
  readonly ordinal: number;
  readonly text: string;
  /** Locale of `text`; `en` for the primary corpus. */
  readonly locale: string;
  /** Source anchor ids that back this chunk. Never empty. */
  readonly sourceIds: readonly string[];
  /** `domainDigest(chunk, [id, articleId, kind, ordinal, locale, text, ...sources])`. */
  readonly digest: string;
};

export type HelpCanonicalArticle = {
  readonly id: string;
  readonly title: string;
  readonly topic: HelpTopic;
  readonly summary: string;
  readonly body: string;
  /** Natural-language phrasings a user might type. */
  readonly aliases: readonly string[];
  /** Expert / identifier terminology. */
  readonly keywords: readonly string[];
  readonly sources: readonly HelpSourceAnchor[];
  readonly audience: readonly HelpAudience[];
  readonly access: HelpAccess;
  readonly capabilityIds: readonly string[];
  /** Locales this article is genuinely retrievable in, beyond `en`. */
  readonly localizations: readonly HelpLocalization[];
  /**
   * Legacy `grokptah.help.v1` entry id this article projects to, when the
   * consolidated article replaces a former standalone entry.
   */
  readonly legacyEntryId?: string;
  /** Digest over this article's own content and its source digests. */
  readonly digest: string;
};

/** The frozen, digest-bound corpus handed to every retriever. */
export type HelpCanonicalCorpus = {
  readonly schemaVersion: typeof HELP_CANONICAL_SCHEMA_VERSION;
  readonly contentVersion: typeof HELP_CANONICAL_CONTENT_VERSION;
  readonly articles: readonly HelpCanonicalArticle[];
  readonly chunks: readonly HelpChunk[];
  /** Every distinct source anchor, sorted by id. */
  readonly sources: readonly HelpSourceAnchor[];
  /**
   * `sha256:` digest over the canonical serialization of the content.
   * Retrieval, the model artifact, and the answer contract are all bound to
   * this value and fail closed when it drifts.
   */
  readonly digest: string;
  /**
   * Digest over the set of per-source digests, for anchor drift checks.
   *
   * Computed from length-prefixed per-source digests rather than joined
   * `id|path#heading` strings; the joined form collided whenever a separator
   * character appeared inside a field.
   */
  readonly sourceDigest: string;
};

/**
 * Authoring shape. Articles reference sources by id so a citation id always
 * resolves to exactly one `path#heading`; `corpus.ts` resolves them.
 */
export type HelpArticleSeed = Omit<HelpCanonicalArticle, "sources" | "digest"> & {
  readonly sourceIds: readonly string[];
};
