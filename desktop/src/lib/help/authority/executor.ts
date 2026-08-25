/**
 * Authorized Help executor.
 *
 * Retrieval alone has no notion of who is asking. This layer sits between a
 * caller and `searchHelpCorpus`, authorizes the action against the principal
 * at the moment of the action, and drops any result whose source was denied.
 *
 * Both transports use this same path:
 *   - the desktop Tauri command (`desktop/src-tauri/src/help.rs`) authorizes
 *     with the Rust crate, which is proven equivalent to the TypeScript
 *     mirror by the shared fixture set;
 *   - the authenticated browser broker calls `createHelpExecutor` directly.
 *
 * The executor is deliberately thin: it constructs a decision request,
 * delegates the decision, and enforces it. Every authorization rule lives in
 * one place, so the two transports cannot develop different behavior.
 */
import { HELP_CORPUS, HELP_CORPUS_DIGEST, getHelpArticle } from "../canonical/corpus";
import {
  HELP_INDEX_PROVENANCE,
  searchHelpCorpus,
  type HelpRetrievalOptions,
  type HelpRetrievalOutcome,
  type HelpRetrievalResult,
} from "../retrieval/hybrid";
import {
  HELP_DECISION_REQUEST_SCHEMA,
  authorizeHelpDecision,
  type HelpDecisionRequest,
  type HelpDecisionResponse,
  type HelpPrincipal,
  type HelpSourceDescriptor,
} from "./decision";

export type HelpExecutorOutcome = {
  /** The authorization decision that produced this outcome. */
  readonly decision: HelpDecisionResponse;
  /** Results the principal is permitted to see. Empty when denied. */
  readonly outcome: HelpRetrievalOutcome;
  /** Result count removed because their sources were denied. */
  readonly withheldCount: number;
};

export type HelpExecutor = {
  search: (
    principal: HelpPrincipal,
    query: string,
    options?: HelpRetrievalOptions,
  ) => HelpExecutorOutcome;
  /** The decision request the executor would build. Exposed for parity tests. */
  buildDecisionRequest: (
    principal: HelpPrincipal,
    action: HelpDecisionRequest["action"],
    sources: readonly HelpSourceDescriptor[],
  ) => HelpDecisionRequest;
};

/**
 * Describe every source the corpus could surface for this tenant.
 *
 * Descriptors come from the corpus, never from the caller: a caller that
 * supplied its own descriptors could relabel a private source as public and
 * authorize itself.
 */
function describeCorpusSources(tenantId: string): HelpSourceDescriptor[] {
  const seen = new Map<string, HelpSourceDescriptor>();
  for (const article of HELP_CORPUS.articles) {
    for (const source of article.sources) {
      if (seen.has(source.id)) continue;
      seen.set(source.id, {
        source_id: source.id,
        visibility: source.visibility,
        // Public documentation belongs to the querying tenant for the purpose
        // of the check; project and private sources carry their real owner
        // once the corpus gains any.
        tenant_id: tenantId,
        digest: source.digest,
      });
    }
  }
  return [...seen.values()].sort((left, right) =>
    left.source_id < right.source_id ? -1 : left.source_id > right.source_id ? 1 : 0,
  );
}

/** Empty outcome carrying the same provenance a real one would. */
function deniedOutcome(query: string): HelpRetrievalOutcome {
  return Object.freeze({
    schema: "grokptah.help-retrieval.v1",
    corpusDigest: HELP_CORPUS_DIGEST,
    corpusContentVersion: HELP_CORPUS.contentVersion,
    modelId: HELP_INDEX_PROVENANCE.modelId,
    indexDigest: HELP_INDEX_PROVENANCE.indexDigest,
    mode: "hybrid",
    query,
    results: Object.freeze([]),
    abstained: true,
    abstentionReason: "no-match",
    confidence: 0,
    margin: 0,
    queryTruncated: false,
    queryFamiliarity: 0,
    corrections: Object.freeze([]),
    redactions: Object.freeze([]),
  }) as HelpRetrievalOutcome;
}

export function createHelpExecutor(): HelpExecutor {
  const buildDecisionRequest = (
    principal: HelpPrincipal,
    action: HelpDecisionRequest["action"],
    sources: readonly HelpSourceDescriptor[],
  ): HelpDecisionRequest =>
    Object.freeze({
      schema: HELP_DECISION_REQUEST_SCHEMA,
      action,
      principal,
      corpus_digest: HELP_CORPUS_DIGEST,
      index_digest: HELP_INDEX_PROVENANCE.indexDigest,
      sources: Object.freeze([...sources]),
    });

  return {
    buildDecisionRequest,

    search(principal, query, options = {}) {
      const sources = describeCorpusSources(principal.tenant_id);
      const request = buildDecisionRequest(principal, "search", sources);
      const decision = authorizeHelpDecision(
        request,
        HELP_CORPUS_DIGEST,
        HELP_INDEX_PROVENANCE.indexDigest,
      );

      if (!decision.allowed) {
        // A denied action returns nothing at all, not a filtered view.
        return { decision, outcome: deniedOutcome(query), withheldCount: 0 };
      }

      const allowed = new Set(decision.receipt.allowed_source_ids);
      const outcome = searchHelpCorpus(query, options);
      // Enforcement is applied to the result set, not merely reported: a
      // result whose citations rest on a denied source is removed.
      const permitted: HelpRetrievalResult[] = [];
      let withheldCount = 0;
      for (const result of outcome.results) {
        const article = getHelpArticle(result.articleId);
        const backed =
          article !== undefined && article.sources.every((source) => allowed.has(source.id));
        if (backed) permitted.push(result);
        else withheldCount += 1;
      }

      if (withheldCount === 0) return { decision, outcome, withheldCount };
      return {
        decision,
        outcome: Object.freeze({
          ...outcome,
          results: Object.freeze(permitted.map((result, index) => Object.freeze({ ...result, rank: index + 1 }))),
          abstained: permitted.length === 0,
          abstentionReason: permitted.length === 0 ? "no-match" : outcome.abstentionReason,
        }) as HelpRetrievalOutcome,
        withheldCount,
      };
    },
  };
}
