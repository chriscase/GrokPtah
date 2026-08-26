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
// The canonical Help core: corpus, offline hybrid retrieval, the bounded
// answer contract, and a framework-agnostic controller. React primitives are
// published separately at `@grokptah/client/help-react` so this entry stays
// dependency-free.
export * from "./help/publicSurface";
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
