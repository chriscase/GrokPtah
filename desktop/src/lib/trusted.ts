/**
 * Trusted-adapter entry point. This barrel may use a direct MCP bearer token
 * and must not be shipped in a browser bundle. Browser consumers use `public`.
 */
export * from "./capabilities";
export * from "./grokptahClient";
export * from "./grokptahOperations";
export * from "./help";
