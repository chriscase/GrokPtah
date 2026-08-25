/**
 * Headless, Tauri-free UI primitives for external products.
 *
 * This barrel intentionally exports state reducers and pure stream helpers,
 * not React components or desktop adapters. It is the staging boundary for a
 * future `@grokptah/ui-core` package that ContextDesk and other consumers can
 * use with their own visual language.
 */
export * from "./capabilities";
export * from "./help";
export * from "./promptQueue";
export * from "./streamApply";
