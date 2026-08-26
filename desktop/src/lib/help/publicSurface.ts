/**
 * What `@grokptah/client` is allowed to ship.
 *
 * The internal barrel (`./index`) is what the desktop and the tests import. It
 * is not what a browser consumer should be handed, and the difference is the
 * point of this file.
 *
 * Two things are deliberately absent:
 *
 * 1. **No way to build a request and send it somewhere.**
 *    `buildHelpAnswerRequest` and `validateHelpAnswerRequest` are contract
 *    plumbing that `askHelp` drives internally. Publishing them would let a
 *    consumer assemble the payload and hand it to an endpoint of its choosing,
 *    from a bundle carrying GrokPtah's name.
 * 2. **No authority, in any form.** There is nothing here to authorize with,
 *    because a decision made inside the consumer's own bundle is a decision the
 *    consumer can decline to make. `askHelp` is published, and it does nothing
 *    at all unless the host bound an authority across the seam.
 *
 * What *is* published divides cleanly:
 *
 * - **Read the corpus.** Every source it ships is public; `verify-public.mjs`
 *   asserts that rather than assuming it.
 * - **Retrieve offline.** Search, rank, excerpt, redact, and abstain, entirely
 *   in the consumer's process. This is the product, not a degraded mode.
 * - **Verify what came back.** Spans, claim coverage, and response validation
 *   are checks that can only make a consumer stricter. A citation that cannot
 *   be re-checked by someone who did not produce it is not a citation, so the
 *   means to re-check it belongs here.
 * - **Render.** Headless controller, React primitives, and the a11y helpers.
 */

// ---- canonical corpus -----------------------------------------------------
export {
  HELP_CORPUS,
  HELP_CORPUS_DIGEST,
  HELP_CHUNK_MAX_CHARS,
  getHelpArticle,
  getHelpChunk,
  getHelpSource,
  recomputeHelpCorpusDigest,
  serializeHelpCorpus,
} from "./canonical/corpus";
export {
  HELP_CANONICAL_CONTENT_VERSION,
  HELP_CANONICAL_SCHEMA_VERSION,
  type HelpAccess,
  type HelpAudience,
  type HelpCanonicalArticle,
  type HelpCanonicalCorpus,
  type HelpChunk,
  type HelpLocalization,
  type HelpSourceAnchor,
  type HelpTopic,
} from "./canonical/types";
export { canonicalDigest, canonicalJson, domainDigest, sha256Hex } from "./canonical/digest";

// ---- offline retrieval ----------------------------------------------------
export {
  HELP_ABSTENTION_THRESHOLD,
  HELP_COORDINATION_EXPONENT,
  HELP_FUSION_WEIGHTS,
  HELP_RETRIEVAL_DEFAULT_LIMIT,
  HELP_RETRIEVAL_MAX_LIMIT,
  HELP_RETRIEVAL_SCHEMA,
  HelpCorpusDigestMismatchError,
  searchHelpCorpus,
  type HelpAbstentionReason,
  type HelpCitation,
  type HelpRetrievalMode,
  type HelpRetrievalOptions,
  type HelpRetrievalOutcome,
  type HelpRetrievalResult,
  type HelpScoreComponents,
} from "./retrieval/hybrid";
export {
  HELP_EXCERPT_MAX_CHARS,
  buildHelpExcerpt,
  sanitizeHelpText,
  type HelpExcerpt,
  type HelpHighlight,
} from "./retrieval/highlight";
export {
  HELP_REDACTION_PLACEHOLDER,
  containsHelpSecret,
  redactHelpText,
  scanHelpForSecrets,
  type HelpRedaction,
  type HelpRedactionResult,
  type HelpSecretConfidence,
  type HelpSecretScan,
} from "./retrieval/redact";
export {
  HELP_MODEL_ID,
  HELP_MODEL_PROVENANCE,
  HELP_MODEL_STATS,
  verifyHelpModelChecksum,
  type HelpModelProvenance,
} from "./model/artifact";

// ---- verification ---------------------------------------------------------
// Checks, not authority: every one of these can only cause a consumer to
// accept less than it otherwise would.
export {
  HELP_MAX_QUOTE_CODE_POINTS,
  buildHelpClaimSpan,
  helpSpansOverlap,
  mapSanitizedRangeToSource,
  sanitizeWithOffsetMap,
  verifyHelpClaimSpan,
  type HelpClaimSpan,
  type HelpSpanFailure,
  type HelpSpanVerification,
} from "./retrieval/spans";
export {
  HELP_CLAIM_MIN_TOKENS_FOR_RELEVANCE,
  HELP_CLAIM_SUPPORT_FRACTION,
  HELP_MAX_CLAIMS,
  checkHelpClaimCoverage,
  segmentHelpClaims,
  type HelpAnswerClaim,
  type HelpCoverageFailure,
  type HelpCoverageResult,
} from "./answer/claims";

// ---- asking, only across a host-bound seam --------------------------------
export {
  HELP_ANSWER_LIMITS,
  HELP_ANSWER_REQUEST_SCHEMA,
  HELP_ANSWER_RESPONSE_SCHEMA,
  askHelp,
  validateHelpAnswerResponse,
  type HelpAnswerCitation,
  type HelpAnswerContextChunk,
  type HelpAnswerFailure,
  type HelpAnswerOptions,
  type HelpAnswerOutcome,
  type HelpAnswerRejection,
  type HelpAnswerRequest,
  type HelpAnswerResponse,
  type HelpAnswerValidation,
} from "./answer/contract";
export {
  HELP_NO_AUTHORITY,
  type HelpAnswerAuthority,
  type HelpAnswerAuthorityResult,
  type HelpAnswerExecution,
  type HelpAnswerRefusal,
} from "./answer/seam";

// ---- headless consumer ----------------------------------------------------
export {
  createHelpSearchController,
  describeHelpResultForAssistiveTech,
  type HelpSearchController,
  type HelpSearchState,
} from "./consumer";
