/**
 * What `@grokptah/client` is allowed to ship.
 *
 * The published package previously re-exported the whole Help barrel. That
 * shipped the local authority — a browser consumer could import the decision
 * functions and decide, in code it controls, whether it was allowed to see a
 * source. A decision made by the party it constrains is not a decision. It
 * also shipped the local transport, so a consumer could point the answer
 * contract at any endpoint it liked in a bundle carrying GrokPtah's name.
 *
 * This surface is what remains once both are gone:
 *
 * - **Read the public corpus.** `help-corpus-public.v1.json` is emitted by
 *   `help-codegen` from the same builder that produced the full corpus, so the
 *   filtering is done by the code that owns the data rather than by a
 *   packaging step that could be skipped. `assertPublicOnly` re-checks it at
 *   load rather than trusting the build.
 * - **Retrieve offline.** Search, rank, and abstain, entirely in the
 *   consumer's process. This is the product, not a degraded mode.
 * - **Verify what a server returned.** A citation that cannot be re-checked by
 *   someone who did not produce it is not a citation, so the means to
 *   re-check one belongs here. These checks can only make a consumer stricter.
 * - **Render.** Types and pure helpers with no authority in them.
 *
 * Deliberately absent, and asserted absent by `helpPublicSurface.test.ts`:
 * authority constructors, route selection, transport (`helpAsk`,
 * `helpFollow`, `helpCancel` and the Tauri `invoke` they use), raw provider
 * replies, the private corpus, and the executor. Authorization and execution
 * reach a server; neither is importable from here.
 */

import type { HelpCorpus } from "./generated/contract";
import { HELP_PUBLIC_CORPUS, HELP_PUBLIC_CORPUS_DIGEST } from "./canonical/corpus";

/** Raised when a bundle that must be public-only is not. */
export class HelpBundleNotPublicError extends Error {
  constructor(readonly record: string) {
    super(
      `Help bundle contains the non-public record ${record}. ` +
        `A published bundle that carries a restricted source has leaked it to every ` +
        `consumer at once, so this bundle is not usable.`,
    );
    this.name = "HelpBundleNotPublicError";
  }
}

/**
 * Throw unless every record in `corpus` is public.
 *
 * Run at module load below. The build already filters, but a check that costs
 * microseconds and catches a mis-shipped bundle is worth running at the point
 * where the bundle would otherwise be used.
 */
export function assertPublicOnly(corpus: HelpCorpus): void {
  for (const source of corpus.sources) {
    if (source.visibility !== "public") throw new HelpBundleNotPublicError(`source:${source.id}`);
  }
  for (const article of corpus.articles) {
    if (article.visibility !== "public") {
      throw new HelpBundleNotPublicError(`article:${article.id}`);
    }
  }
  for (const chunk of corpus.chunks) {
    if (chunk.visibility !== "public") throw new HelpBundleNotPublicError(`chunk:${chunk.id}`);
  }
}

// Verified at load inside `canonical/corpus`; re-checked here for the property
// that matters to a *published* bundle specifically.
assertPublicOnly(HELP_PUBLIC_CORPUS);

export { HELP_PUBLIC_CORPUS, HELP_PUBLIC_CORPUS_DIGEST };

// ---- offline retrieval ----------------------------------------------------
export {
  HELP_ABSTENTION_THRESHOLD,
  HELP_FUSION_WEIGHTS,
  HELP_RETRIEVAL_DEFAULT_LIMIT,
  HELP_RETRIEVAL_MAX_LIMIT,
  searchHelpCorpus,
  type HelpAbstentionReason,
  type HelpRetrievalMode,
  type HelpRetrievalOptions,
  type HelpRetrievalOutcome,
  type HelpRetrievalResult,
  type HelpScoreComponents,
} from "./retrieval/hybrid";

// ---- verification (can only make a consumer stricter) ---------------------
export {
  verifyHelpProjection,
  type HelpClaimRejection,
  type HelpVerification,
} from "./verify";
export { HelpCorpusDigestMismatchError, isPublicOnly, verifyHelpCorpus } from "./canonical/verify";
export { HELP_DIGEST_DOMAINS, domainDigest, lengthPrefixed, sha256Hex } from "./canonical/digest";

// ---- safe projections and rendering types --------------------------------
export type {
  HelpArticle,
  HelpBoundsProjection,
  HelpChunk,
  HelpChunkKind,
  HelpCitationProjection,
  HelpClaimProjection,
  HelpCorpus,
  HelpProjection,
  HelpProjectionStatus,
  HelpPublicErrorCode,
  HelpReceiptProjection,
  HelpRedactionCount,
  HelpRedactionKind,
  HelpSourceAnchor,
  HelpTopic,
  HelpVisibility,
} from "./generated/contract";
