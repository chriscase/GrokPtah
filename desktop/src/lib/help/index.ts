/**
 * Public, headless Help core.
 *
 * Tauri-free, React-free, dependency-free, and fully offline. A consumer such
 * as ContextDesk can retrieve, rank, cite, and (optionally) phrase Help
 * answers without importing any desktop internals.
 *
 * React primitives live at `./react` so this barrel stays usable from a
 * non-React host and from the dependency-free `@grokptah/client` bundle.
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
  type HelpTopic,
} from "./canonical/types";
export { canonicalDigest, canonicalJson, sha256Hex } from "./canonical/digest";

// ---- retrieval ------------------------------------------------------------
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
  type HelpRedaction,
  type HelpRedactionResult,
} from "./retrieval/redact";
export {
  HELP_QUERY_MAX_CHARS,
  HELP_QUERY_MAX_TERMS,
  boundQuery,
  tokenize,
} from "./retrieval/text";

// ---- embedding model ------------------------------------------------------
export {
  HELP_MODEL_ID,
  HELP_MODEL_PROVENANCE,
  HELP_MODEL_STATS,
  verifyHelpModelChecksum,
} from "./model/artifact";

// ---- bounded answer contract ---------------------------------------------
export {
  HELP_ANSWER_LIMITS,
  HELP_ANSWER_REQUEST_SCHEMA,
  HELP_ANSWER_RESPONSE_SCHEMA,
  buildHelpAnswerRequest,
  createHelpAnswerRoute,
  isHelpAnswerRouteIntact,
  requestHelpAnswer,
  validateHelpAnswerRequest,
  validateHelpAnswerResponse,
  type HelpAnswerCitation,
  type HelpAnswerContextChunk,
  type HelpAnswerFailure,
  type HelpAnswerOptions,
  type HelpAnswerOutcome,
  type HelpAnswerRejection,
  type HelpAnswerRequest,
  type HelpAnswerResponse,
  type HelpAnswerRoute,
  type HelpAnswerTransport,
  type HelpAnswerValidation,
} from "./answer/contract";

// ---- headless consumer ----------------------------------------------------
export {
  createHelpSearchController,
  describeHelpResultForAssistiveTech,
  type HelpSearchController,
  type HelpSearchState,
} from "./consumer";

// Production one-shot authority boundary. This is intentionally separate from
// the legacy Help/Chat projections above.
export * from "./authority/index";
