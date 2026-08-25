/**
 * Headless, Tauri-free UI primitives for external products.
 *
 * This barrel intentionally exports state reducers and pure stream helpers,
 * not React components or desktop adapters. It is the staging boundary for a
 * future `@grokptah/ui-core` package that ContextDesk and other consumers can
 * use with their own visual language.
 *
 * Visual continuity is a separate, narrower contract: `npm run build:public`
 * stages the marked token regions of `src/styles/app.css` as
 * `@grokptah/client/styles/tokens.css`. That is colours, type scale, focus and
 * the screen-reader utility — not components, and not focus management.
 *
 * ## One Help corpus
 *
 * This surface used to export two differently-gated help systems, and the more
 * inviting name (`searchHelp`) resolved to `help.ts` — the capability-gated
 * `grokptah.help.v1` corpus, which no desktop surface displays. A consumer
 * reaching for the obvious name got different help text than the desktop shows.
 *
 * Browser consumers now see exactly one corpus: the live, source-cited
 * `helpCenter` corpus the desktop actually renders. `searchHelp` and
 * `searchHelpArticles` are the *same binding*, so neither name can drift onto a
 * different corpus. `help.ts` remains the access-gated contract for trusted
 * adapters and is reachable only through `trusted.ts`, which is never shipped
 * in a browser bundle.
 */
export * from "./capabilities";
export * from "./externalWorker";
export {
  HELP_ARTICLES,
  HELP_CORPUS_VERSION,
  HELP_INDEX,
  buildHelpAssistantRequest,
  buildHelpSemanticRequest,
  parseHelpAssistantAnswer,
  parseHelpSemanticAnswer,
  validateHelpAssistantAnswer,
  validateHelpSemanticAnswer,
  // One corpus, two names: `searchHelpArticles` is retained for consumers that
  // already bound the explicit name, and is identical to `searchHelp`.
  searchHelp,
  searchHelp as searchHelpArticles,
} from "./helpCenter";
export type {
  HelpArticle,
  HelpAssistantAnswer,
  HelpAssistantRequest,
  HelpAssistantValidation,
  HelpSemanticAnswer,
  HelpSemanticRequest,
  HelpSemanticValidation,
  HelpSource,
  HelpSearchResult,
  HelpSearchResult as HelpArticleSearchResult,
  HelpTopic,
} from "./helpCenter";
export * from "./promptQueue";
export * from "./streamApply";
