/**
 * Public, Tauri-free integration surface.
 *
 * Keep this barrel limited to transport-neutral contracts and clients so it
 * can become the source for a published `@grokptah/client` package.
 */
export * from "./capabilities";
export * from "./grokptahClient";
export * from "./grokptahOperations";
export * from "./grokptahBrokerClient";
