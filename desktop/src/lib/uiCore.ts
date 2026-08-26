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
// Semantic Help ships its offline half only: the public corpus, retrieval,
// and the checks a consumer can re-run. Authority constructors, route
// selection, transport, and the executor are not exported here and are not
// importable from the published package — see `help/publicSurface.ts`.
export * from "./help/publicSurface";
export * from "./promptQueue";
export * from "./streamApply";
