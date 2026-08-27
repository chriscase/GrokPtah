/**
 * Headless, Tauri-free UI primitives for external products.
 *
 * This barrel intentionally exports state reducers and pure stream helpers,
 * not React components or desktop adapters. It is the staging boundary for a
 * future `@grokptah/ui-core` package that ContextDesk and other consumers can
 * use with their own visual language.
 */
export * from "./capabilities";
export * from "./externalWorker";
export * from "./help";
// The canonical, source-cited Help authority: one corpus with stable article
// IDs, a digest manifest, offline hybrid retrieval with citations and
// abstention, and the bounded optional answer seam built on top of it. Both
// modules are transport-free and grant no capability.
export * from "./helpAuthority";
export * from "./helpAnswer";
// The consumer contract above the authority: one presentation status per
// retrieval outcome, verified citation spans, capability and access labels
// that never assert live availability, and the wording for the optional
// model seam's unknowns and timeout. React-free, so a product with its own
// visual language renders the same states the desktop Help Center renders.
export * from "./helpCenterView";
// The source-cited Help Center corpus is exported under explicit names so the
// original bounded `searchHelp` contract remains backward compatible while
// consumers can opt into semantic-ranking and assistant request validation.
export {
  HELP_ARTICLES,
  HELP_CORPUS_VERSION,
  buildHelpAssistantRequest,
  buildHelpSemanticRequest,
  parseHelpAssistantAnswer,
  parseHelpSemanticAnswer,
  searchHelp as searchHelpArticles,
  validateHelpAssistantAnswer,
  validateHelpSemanticAnswer,
} from "./helpCenter";
export type {
  HelpArticle,
  HelpSource,
  HelpAssistantAnswer,
  HelpAssistantRequest,
  HelpAssistantValidation,
  HelpSemanticAnswer,
  HelpSemanticRequest,
  HelpSemanticValidation,
  HelpSearchResult as HelpArticleSearchResult,
  HelpTopic,
} from "./helpCenter";
export * from "./promptQueue";
export * from "./streamApply";
