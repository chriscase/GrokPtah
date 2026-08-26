/**
 * `@grokptah/client/host` - the trusted-host seam.
 *
 * This barrel is bearer-capable and Tauri-free. It is the only supported way
 * for another trusted desktop or server product (ContextDesk and peers) to
 * consume GrokPtah's authenticated powers, and it is deliberately kept out of
 * the browser-safe root and `./ui-core` exports: the published manifest fences
 * this subpath off under the `browser` and `worker` export conditions, so a
 * browser bundler cannot resolve it at all.
 *
 * Nothing here re-implements the capability lattice or the operation lattice;
 * the seam re-exports them and adds a scope-fenced facade on top.
 */
export * from "./capabilities";
export * from "./grokptahClient";
export * from "./grokptahOperations";
export * from "./trustedHost";
