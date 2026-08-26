/**
 * Trusted-adapter entry point. This barrel may use a direct MCP bearer token
 * and must not be shipped in a browser bundle. Browser consumers use `public`.
 *
 * The bearer-capable half is re-exported from `./host`, the module that backs
 * the published `@grokptah/client/host` seam, so the in-repo adapter and the
 * external trusted-host consumers cannot drift apart.
 */
export * from "./host";
export * from "./help";
