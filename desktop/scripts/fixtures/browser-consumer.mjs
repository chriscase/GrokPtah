/**
 * External browser/public consumer fixture.
 *
 * Runs under an explicit `browser` (or `worker`) resolver condition, the way a
 * web bundler resolves the package. The browser-safe entries must keep working
 * unchanged, and the trusted-host seam must be unreachable - not merely
 * unused, but unresolvable.
 */
import assert from "node:assert/strict";

const condition = process.argv[2] ?? "browser";

const publicApi = await import("@grokptah/client");
const uiCoreApi = await import("@grokptah/client/ui-core");

// The browser-safe contract still works exactly as published.
assert.equal(typeof publicApi.GrokPtahBrokerClient, "function");
assert.equal(publicApi.HELP_CONTRACT, "grokptah.help.v1");
assert.equal(publicApi.CAPABILITY_CONTRACT, "grokptah.capabilities.v1");
assert.equal(publicApi.EXTERNAL_WORKER_CONTRACT, "grokptah.external-workers.v1");
assert.equal(typeof publicApi.parseCapabilitySet, "function");
assert.equal(typeof uiCoreApi.promptQueueReducer, "function");
assert.ok(publicApi.HELP_ARTICLES.length > 0);
assert.equal(uiCoreApi.HELP_ARTICLES.length, publicApi.HELP_ARTICLES.length);
assert.equal(
  publicApi.searchHelpArticles("restricted company gateway")[0]?.article?.id,
  "providers.restricted-gateway-review",
);

// Capability and scope types stay usable from the browser-safe surface.
const set = publicApi.parseCapabilitySet({
  contract: publicApi.CAPABILITY_CONTRACT,
  capabilities: [
    {
      id: "run.promote",
      tier: "promote",
      mutating: true,
      human_gate: true,
      availability: "gated",
      description: "Promote isolated runs",
    },
  ],
});
assert.ok(set, "browser consumer could not parse the capability contract");
assert.equal(publicApi.capabilityActionState(publicApi.findCapability(set, "run.promote")), "requires_gate");

// The trusted-host seam must not resolve under a browser-class condition.
let resolutionError = null;
try {
  await import("@grokptah/client/host");
} catch (error) {
  resolutionError = error;
}
assert.ok(
  resolutionError,
  `@grokptah/client/host resolved under the ${condition} condition; the seam is not fenced`,
);
assert.equal(
  resolutionError.code,
  "ERR_PACKAGE_PATH_NOT_EXPORTED",
  `expected an exports fence under ${condition}, got ${resolutionError.code}: ${resolutionError.message}`,
);

console.log(`browser consumer fixture passed under the ${condition} condition`);
