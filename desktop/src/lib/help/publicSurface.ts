/**
 * What `@grokptah/client` is allowed to ship.
 *
 * The published package used to re-export the whole Help barrel, which meant
 * it shipped `authorizeHelpDecision`, `authorizeHelpDecisionJson`,
 * `parseHelpDecisionRequest`, and `createHelpExecutor` — the *local authority*.
 * A browser consumer could import them and decide, in code it controls,
 * whether it was allowed to see a source. That is a decision made by the party
 * it constrains, which is not a decision.
 *
 * It also shipped the local transport: `requestHelpAnswer` and
 * `HelpAnswerTransport`. A consumer could point the answer contract at any
 * endpoint it liked, in a bundle that carries GrokPtah's name.
 *
 * This surface is what remains once both are removed:
 *
 * - **Read the corpus.** The published corpus contains public sources only,
 *   which `scripts/verify-public.mjs` asserts rather than assumes.
 * - **Retrieve offline.** Search, rank, excerpt, redact, and abstain, entirely
 *   in the consumer's process. This is the product, not a degraded mode.
 * - **Verify what a server returned.** Spans, claim coverage, and response
 *   validation are checks that can only make a consumer stricter. A citation
 *   that cannot be re-checked by someone who did not produce it is not a
 *   citation, so the means to re-check it belongs here.
 * - **Schedule and render.** The task runtime and the a11y helpers are UI
 *   concerns with no authority in them.
 *
 * Authorization and answer execution reach the server: the desktop through the
 * Tauri commands, the browser through the authenticated broker client. Neither
 * is importable from here.
 */

// ---- canonical corpus -----------------------------------------------------
export {
  HELP_CORPUS,
  HELP_CORPUS_DIGEST,
  HELP_CHUNK_MAX_CHARS,
  getHelpArticle,
  getHelpChunk,
  getHelpSource,
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
  type HelpSourceVisibility,
  type HelpTopic,
} from "./canonical/types";
export { canonicalDigest, canonicalJson, domainDigest, sha256Hex } from "./canonical/digest";

// ---- offline retrieval ----------------------------------------------------
export {
  HELP_ABSTENTION_THRESHOLD,
  HELP_COORDINATION_EXPONENT,
  HELP_FUSION_WEIGHTS,
  HELP_INDEX_PROVENANCE,
  HELP_RETRIEVAL_DEFAULT_LIMIT,
  HELP_RETRIEVAL_MAX_LIMIT,
  HELP_RETRIEVAL_SCHEMA,
  HelpCorpusDigestMismatchError,
  HelpIndexDigestMismatchError,
  searchHelpCorpus,
  type HelpAbstentionReason,
  type HelpCitation,
  type HelpRetrievalMode,
  type HelpRetrievalOptions,
  type HelpRetrievalOutcome,
  type HelpRetrievalResult,
  type HelpScoreComponents,
} from "./retrieval/hybrid";
export { HELP_INDEX_SCHEMA, type HelpIndexProvenance } from "./retrieval/provenance";
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
export {
  HELP_ANSWER_ADMISSION_SCHEMA,
  HELP_ANSWER_LIMITS,
  HELP_ANSWER_REQUEST_SCHEMA,
  HELP_ANSWER_RESPONSE_SCHEMA,
  validateHelpAnswerResponse,
  type HelpAnswerAdmission,
  type HelpAnswerCitation,
  type HelpAnswerContextChunk,
  type HelpAnswerRejection,
  type HelpAnswerRequest,
  type HelpAnswerRequestCore,
  type HelpAnswerResponse,
  type HelpAnswerRoute,
  type HelpAnswerValidation,
} from "./answer/contract";

// ---- headless consumer and UI scheduling ----------------------------------
export {
  createHelpSearchController,
  describeHelpResultForAssistiveTech,
  type HelpSearchController,
  type HelpSearchState,
} from "./consumer";
export {
  HELP_SCHEDULER_DEFAULTS,
  HelpTaskError,
  createHelpTaskScheduler,
  type HelpSchedulerOptions,
  type HelpTaskContext,
  type HelpTaskFailure,
  type HelpTaskKind,
  type HelpTaskRecord,
  type HelpTaskScheduler,
  type HelpTaskState,
} from "./runtime/tasks";
