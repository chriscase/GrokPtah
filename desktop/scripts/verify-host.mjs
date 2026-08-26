/**
 * Verify the trusted-host seam against the staged package artifacts.
 *
 * This is the authority check for the boundary itself: that the browser-safe
 * root and `./ui-core` surfaces are byte-for-byte the same contract they were
 * before the seam existed, that `./host` is fenced off from browser and worker
 * resolvers, that no bearer-capable identifier reaches a browser bundle, and
 * that the seam fails closed on unsupported capabilities and malformed scope.
 */
import { readFile } from "node:fs/promises";

const publicBundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const uiCoreBundlePath = new URL("../dist/public/ui-core.js", import.meta.url);
const hostBundlePath = new URL("../dist/public/grokptah-host.js", import.meta.url);

const publicBundle = await readFile(publicBundlePath, "utf8");
const uiCoreBundle = await readFile(uiCoreBundlePath, "utf8");
const hostBundle = await readFile(hostBundlePath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);

function fail(message) {
  throw new Error(message);
}

function assertDeepEqual(actual, expected, label) {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    fail(`${label} changed: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

// 1. The browser-safe entries must be exactly what they were before the seam.
assertDeepEqual(
  manifest.exports?.["."],
  { types: "./types/public.d.ts", import: "./grokptah-public.js" },
  "public root export",
);
assertDeepEqual(
  manifest.exports?.["./ui-core"],
  { types: "./types/uiCore.d.ts", import: "./ui-core.js" },
  "ui-core export",
);
if (manifest.name !== "@grokptah/client" || manifest.type !== "module") {
  fail("public package identity changed");
}

// 2. The trusted-host subpath must be present and fenced.
const hostExport = manifest.exports?.["./host"];
if (!hostExport || typeof hostExport !== "object") {
  fail("public package manifest does not expose the ./host trusted seam");
}
for (const condition of ["browser", "worker", "default"]) {
  if (hostExport[condition] !== null) {
    fail(`./host must resolve to null under the ${condition} condition`);
  }
}
if (
  hostExport.types !== "./types/host.d.ts" ||
  hostExport.import !== "./grokptah-host.js"
) {
  fail("./host does not point at the trusted-host bundle and its declarations");
}
const hostConditionOrder = Object.keys(hostExport);
for (const fence of ["browser", "worker"]) {
  if (hostConditionOrder.indexOf(fence) > hostConditionOrder.indexOf("import")) {
    fail(`./host lists ${fence} after import, so a browser resolver would win the match`);
  }
}
if (!manifest.files?.includes("grokptah-host.js")) {
  fail("published files do not include the trusted-host bundle");
}
for (const required of ["grokptah-public.js", "ui-core.js", "types"]) {
  if (!manifest.files?.includes(required)) fail(`published files dropped ${required}`);
}

// 3. No bearer-capable or Tauri-only implementation may reach a browser bundle.
const trustedOnlyMarkers = [
  "GrokPtahClient",
  "GrokPtahOperations",
  "GrokPtahHost",
  "GrokPtahScopeError",
  "streamRunEvents",
  "mcp-session-id",
  "MCP-Protocol-Version",
  "Authorization",
  "Bearer",
  "ptah_submit_task",
  "ptah_approve_run",
  "ptah_promote_run",
  "ptah_resume_persistent_agent",
];
for (const [label, bundle] of [
  ["public", publicBundle],
  ["ui-core", uiCoreBundle],
]) {
  const leaked = trustedOnlyMarkers.filter((needle) => bundle.includes(needle));
  if (leaked.length > 0) {
    fail(`${label} bundle leaked trusted-host markers: ${leaked.join(", ")}`);
  }
}

// 4. The host bundle must stay Tauri-free and free of embedded credentials
//    or host filesystem paths; the bearer token is a caller-supplied value.
const forbiddenInHost = [
  "@tauri-apps",
  "XAI_API_KEY",
  "/Users/",
  "/private/",
  "GROKPTAH_HOME",
  "apiKey",
];
const hostLeaks = forbiddenInHost.filter((needle) => hostBundle.includes(needle));
if (hostLeaks.length > 0) {
  fail(`trusted-host bundle contains forbidden authority markers: ${hostLeaks.join(", ")}`);
}

// 4b. The two declaration emits share one output directory. The browser-safe
//     declaration barrels must never acquire a bearer-capable re-export.
const typesRoot = new URL("../dist/public/types/", import.meta.url);
for (const barrel of ["public.d.ts", "uiCore.d.ts"]) {
  const declaration = await readFile(new URL(barrel, typesRoot), "utf8");
  for (const trustedModule of ["./host", "./trustedHost", "./grokptahClient", "./grokptahOperations"]) {
    if (declaration.includes(`from "${trustedModule}"`)) {
      fail(`${barrel} re-exports the trusted module ${trustedModule}`);
    }
  }
}
const hostDeclaration = await readFile(new URL("host.d.ts", typesRoot), "utf8");
for (const trustedModule of ["./grokptahClient", "./grokptahOperations", "./trustedHost"]) {
  if (!hostDeclaration.includes(`from "${trustedModule}"`)) {
    fail(`host.d.ts does not re-export ${trustedModule}`);
  }
}

const publicApi = await import(publicBundlePath.href);
const uiCoreApi = await import(uiCoreBundlePath.href);
const hostApi = await import(hostBundlePath.href);

// 5. The browser-safe export name sets are pinned, so neither a new trusted
//    symbol nor a dropped public symbol can pass unnoticed.
const PUBLIC_EXPORTS = [
  "CAPABILITY_CONTRACT", "EXTERNAL_WORKER_CONTRACT", "GrokPtahBrokerClient", "GrokPtahBrokerError",
  "HELP_ARTICLES", "HELP_ASSISTANT_MAX_BYTES", "HELP_CONTRACT", "HELP_CORPUS_VERSION",
  "HELP_ENTRIES", "applyAssistantStreamChunk", "applyExternalWorkerNotification",
  "buildHelpAssistantContext", "buildHelpAssistantRequest", "buildHelpSemanticRequest",
  "capabilityActionState", "createExternalWorkerMonitor", "createPromptQueueEntry",
  "drainPromptQueuePrefix", "emptyPromptQueueState", "findCapability", "nextAssistantText",
  "parseBrokerApproval", "parseBrokerBinding", "parseBrokerEventUpdate",
  "parseBrokerReviewProjection", "parseBrokerRun", "parseBrokerRunProjection",
  "parseCapabilitySet", "parseExternalWorkerArtifact", "parseExternalWorkerEvent",
  "parseExternalWorkerFollowUpRequest", "parseExternalWorkerLaunchRequest",
  "parseExternalWorkerLaunchResult", "parseExternalWorkerNotification",
  "parseExternalWorkerRecord", "parseExternalWorkerRunRecord", "parseHelpAssistantAnswer",
  "parseHelpSemanticAnswer", "promptQueueReducer", "queueEntriesFor", "queueKind",
  "queueRevisionFor", "searchHelp", "searchHelpArticles", "shouldStickToBottom",
  "streamVisualDelta", "validateHelpAssistantAnswer", "validateHelpSemanticAnswer",
];
const UI_CORE_EXPORTS = PUBLIC_EXPORTS.filter(
  (name) =>
    ![
      "GrokPtahBrokerClient",
      "GrokPtahBrokerError",
      "parseBrokerApproval",
      "parseBrokerBinding",
      "parseBrokerEventUpdate",
      "parseBrokerReviewProjection",
      "parseBrokerRun",
      "parseBrokerRunProjection",
    ].includes(name),
);
assertDeepEqual(Object.keys(publicApi).sort(), PUBLIC_EXPORTS.slice().sort(), "public export set");
assertDeepEqual(Object.keys(uiCoreApi).sort(), UI_CORE_EXPORTS.slice().sort(), "ui-core export set");

// 6. The seam exposes the trusted powers ContextDesk-class consumers need.
const requiredHostExports = [
  "GROKPTAH_HOST_CONTRACT",
  "GROKPTAH_MAX_ROUNDS",
  "GROKPTAH_RECOVERY_POLL_TOOL",
  "GrokPtahCapabilityError",
  "GrokPtahClient",
  "GrokPtahHost",
  "GrokPtahHostRun",
  "GrokPtahHostWorkspace",
  "GrokPtahOperations",
  "GrokPtahRemoteError",
  "GrokPtahScopeError",
  "applyGrokPtahRunNotification",
  "assertGrokPtahRunScope",
  "assertGrokPtahScope",
  "capabilityActionState",
  "createGrokPtahRunMonitor",
  "findCapability",
  "negotiateGrokPtahCapabilities",
  "parseCapabilitySet",
  "parseGrokPtahRunScope",
  "parseGrokPtahScope",
  "requireGrokPtahCapabilities",
  "validateGrokPtahBounds",
];
const missingHost = requiredHostExports.filter((name) => !(name in hostApi));
if (missingHost.length > 0) {
  fail(`trusted-host bundle is missing required exports: ${missingHost.join(", ")}`);
}
// The seam is a trusted surface, not a second copy of the browser package.
for (const browserOnly of ["GrokPtahBrokerClient", "promptQueueReducer", "HELP_ARTICLES"]) {
  if (browserOnly in hostApi) {
    fail(`trusted-host bundle must not re-publish the browser surface: ${browserOnly}`);
  }
}

// 7. The capability lattice is preserved and negotiation fails closed.
if (hostApi.CAPABILITY_CONTRACT !== publicApi.CAPABILITY_CONTRACT) {
  fail("trusted-host seam negotiates a different capability contract than the public surface");
}
if (hostApi.GROKPTAH_HOST_CONTRACT !== "grokptah.host.v1") {
  fail("trusted-host seam contract marker changed");
}
const descriptor = {
  id: "run.review",
  tier: "review",
  mutating: false,
  human_gate: false,
  availability: "available",
  description: "Read run projections",
};
const gated = {
  id: "run.promote",
  tier: "promote",
  mutating: true,
  human_gate: true,
  availability: "gated",
  description: "Promote isolated runs",
};
const set = hostApi.parseCapabilitySet({
  contract: hostApi.CAPABILITY_CONTRACT,
  capabilities: [descriptor, gated],
});
if (!set) fail("trusted-host seam could not parse a valid capability set");

const report = hostApi.negotiateGrokPtahCapabilities(set, [
  "run.review",
  "run.promote",
  "computer.control",
]);
assertDeepEqual(report.ready, ["run.review"], "negotiated ready set");
assertDeepEqual(report.requiresGate, ["run.promote"], "negotiated gated set");
assertDeepEqual(report.unavailable, ["computer.control"], "negotiated unavailable set");
if (report.contract !== hostApi.CAPABILITY_CONTRACT) fail("negotiation report lost its contract");

// An unsupported capability must refuse, not degrade.
let refusal = null;
try {
  hostApi.requireGrokPtahCapabilities(set, ["computer.control"]);
} catch (error) {
  refusal = error;
}
if (!(refusal instanceof hostApi.GrokPtahCapabilityError) || refusal.state !== "unavailable") {
  fail("unsupported capability did not fail closed with GrokPtahCapabilityError");
}
// A gated capability without its gate must refuse too.
let gateRefusal = null;
try {
  hostApi.requireGrokPtahCapabilities(set, ["run.promote"]);
} catch (error) {
  gateRefusal = error;
}
if (!(gateRefusal instanceof hostApi.GrokPtahCapabilityError) || gateRefusal.state !== "requires_gate") {
  fail("gated capability without an approval did not fail closed");
}
if (hostApi.requireGrokPtahCapabilities(set, [{ id: "run.promote", gateSatisfied: true }]).ready[0] !== "run.promote") {
  fail("a satisfied gate did not admit the gated capability");
}
// A host that never negotiated must be able to do nothing at all.
if (hostApi.negotiateGrokPtahCapabilities(null, ["run.review"]).unavailable[0] !== "run.review") {
  fail("an un-negotiated capability set did not fail closed");
}

// 8. Malformed scope fails closed before any transport call.
const validScope = { sessionId: "session-1", workspace: "workspace-1" };
assertDeepEqual(hostApi.parseGrokPtahScope(validScope), validScope, "parsed workspace scope");
assertDeepEqual(
  hostApi.parseGrokPtahRunScope({ ...validScope, runId: "run-1" }),
  { ...validScope, runId: "run-1" },
  "parsed run scope",
);
const malformedScopes = [
  null,
  "session-1",
  [],
  {},
  { sessionId: "session-1" },
  { sessionId: "session-1", workspace: "" },
  { sessionId: "session-1", workspace: "   " },
  { sessionId: "session-1", workspace: 7 },
  { sessionId: "session-1", workspace: "workspace-1", runId: "run-1" },
  { sessionId: "session-1", workspace: "workspace-1", token: "secret" },
  { sessionId: "session-1", workspace: `workspace${String.fromCharCode(10)}injected` },
  { sessionId: "session-1", workspace: "w".repeat(513) },
];
for (const scope of malformedScopes) {
  if (hostApi.parseGrokPtahScope(scope) !== null) {
    fail(`malformed scope was accepted: ${JSON.stringify(scope)}`);
  }
  let scopeRefusal = null;
  try {
    hostApi.assertGrokPtahScope(scope);
  } catch (error) {
    scopeRefusal = error;
  }
  if (!(scopeRefusal instanceof hostApi.GrokPtahScopeError)) {
    fail(`malformed scope did not raise GrokPtahScopeError: ${JSON.stringify(scope)}`);
  }
}
if (hostApi.parseGrokPtahRunScope(validScope) !== null) {
  fail("a run scope without a runId was accepted");
}

// 9. A host bound to a fake transport must refuse an un-negotiated stream.
const host = new hostApi.GrokPtahHost({
  baseUrl: "https://grokptah.invalid",
  token: "synthetic-token",
  fetcher: async () => {
    throw new Error("verify-host must not reach transport");
  },
});
let streamRefusal = null;
try {
  host.run({ ...validScope, runId: "run-1" }).stream();
} catch (error) {
  streamRefusal = error;
}
if (!(streamRefusal instanceof hostApi.GrokPtahCapabilityError)) {
  fail("streaming without a negotiated capability set did not fail closed");
}

console.log(
  `trusted-host seam verified: ./host fenced under browser/worker, ` +
    `${requiredHostExports.length} exports, public surface unchanged`,
);
