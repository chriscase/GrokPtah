import { readFile } from "node:fs/promises";

const bundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const uiCoreBundlePath = new URL("../dist/public/ui-core.js", import.meta.url);
const helpReactBundlePath = new URL("../dist/public/help-react.js", import.meta.url);
const bundle = await readFile(bundlePath, "utf8");
const uiCoreBundle = await readFile(uiCoreBundlePath, "utf8");
const helpReactBundle = await readFile(helpReactBundlePath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@grokptah/client" ||
  manifest.exports?.["."]?.import !== "./grokptah-public.js" ||
  manifest.exports?.["."]?.types !== "./types/public.d.ts" ||
  manifest.exports?.["./ui-core"]?.import !== "./ui-core.js" ||
  manifest.exports?.["./ui-core"]?.types !== "./types/uiCore.d.ts" ||
  manifest.exports?.["./help-react"]?.import !== "./help-react.js" ||
  manifest.exports?.["./help-react"]?.types !== "./types/helpPublic.d.ts"
) {
  throw new Error("public package manifest does not expose the expected safe entry point");
}
const forbidden = [
  "@tauri-apps",
  "trusted.ts",
  "Authorization: Bearer",
  "XAI_API_KEY",
  "/Users/",
  "/private/",
  "GROKPTAH_HOME",
  "apiKey",
];
const leaked = forbidden.filter((needle) => bundle.includes(needle));
if (leaked.length > 0) {
  throw new Error(`public bundle contains forbidden authority markers: ${leaked.join(", ")}`);
}
const leakedUiCore = forbidden.filter((needle) => uiCoreBundle.includes(needle));
if (leakedUiCore.length > 0) {
  throw new Error(`ui-core bundle contains forbidden authority markers: ${leakedUiCore.join(", ")}`);
}
const leakedHelpReact = forbidden.filter((needle) => helpReactBundle.includes(needle));
if (leakedHelpReact.length > 0) {
  throw new Error(`help-react bundle contains forbidden authority markers: ${leakedHelpReact.join(", ")}`);
}
// The React entry must not inline React, and the dependency-free entries must
// not acquire a React dependency from the shared Help core.
if (/from\s*["']react["']/.test(uiCoreBundle) || /require\(["']react["']\)/.test(uiCoreBundle)) {
  throw new Error("ui-core bundle acquired a React dependency");
}
if (!/from\s*["']react/.test(helpReactBundle)) {
  throw new Error("help-react bundle does not treat React as an external peer");
}
// Nothing in the public surface may render provider or corpus text as HTML.
for (const [name, text] of [["public", bundle], ["ui-core", uiCoreBundle], ["help-react", helpReactBundle]]) {
  if (text.includes("dangerouslySetInnerHTML") || text.includes("innerHTML")) {
    throw new Error(`${name} bundle assigns raw HTML; Help excerpts must stay plain text`);
  }
}

const publicApi = await import(bundlePath.href);
const uiCoreApi = await import(uiCoreBundlePath.href);
const requiredExports = [
  "GrokPtahBrokerClient",
  "parseBrokerApproval",
  "parseBrokerEventUpdate",
  "parseBrokerRunProjection",
  "parseBrokerReviewProjection",
  "EXTERNAL_WORKER_CONTRACT",
  "parseExternalWorkerLaunchRequest",
  "parseExternalWorkerFollowUpRequest",
  "parseExternalWorkerLaunchResult",
  "parseExternalWorkerNotification",
  "applyExternalWorkerNotification",
  "searchHelp",
  "searchHelpArticles",
  "HELP_ARTICLES",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
  // Canonical Help core.
  "HELP_CORPUS",
  "HELP_CORPUS_DIGEST",
  "searchHelpCorpus",
  "createHelpSearchController",
  "describeHelpResultForAssistiveTech",
  "buildHelpAnswerRequest",
  "createHelpAnswerRoute",
  "validateHelpAnswerResponse",
  "requestHelpAnswer",
  "redactHelpText",
  "sanitizeHelpText",
  "verifyHelpModelChecksum",
  // Authority, spans, provenance, and the task runtime.
  "authorizeHelpDecision",
  "parseHelpDecisionRequest",
  "createHelpExecutor",
  "buildHelpClaimSpan",
  "verifyHelpClaimSpan",
  "HELP_INDEX_PROVENANCE",
  "createHelpTaskScheduler",
];
const missing = requiredExports.filter((name) => !(name in publicApi));
if (missing.length > 0) {
  throw new Error(`public bundle is missing required exports: ${missing.join(", ")}`);
}
for (const name of [
  "HELP_ARTICLES",
  "searchHelpArticles",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
  "EXTERNAL_WORKER_CONTRACT",
  "parseExternalWorkerFollowUpRequest",
  "parseExternalWorkerNotification",
  "applyExternalWorkerNotification",
]) {
  if (!(name in uiCoreApi)) throw new Error(`ui-core bundle is missing required export: ${name}`);
}
if ("GrokPtahBrokerClient" in uiCoreApi) {
  throw new Error("ui-core bundle must not expose the browser broker client");
}
if (!Object.isFrozen(publicApi.HELP_ARTICLES) || !Object.isFrozen(uiCoreApi.HELP_ARTICLES)) {
  throw new Error("published Help corpus must be immutable");
}
if (!Object.isFrozen(publicApi.HELP_ENTRIES) || !Object.isFrozen(uiCoreApi.HELP_ENTRIES)) {
  throw new Error("published capability-aware Help corpus must be immutable");
}

const helpHits = publicApi.searchHelp("restricted gateway", {
  audience: "operator",
  includeRestricted: true,
});
if (!helpHits.some(({ entry }) => entry.id === "enterprise-gateway-review")) {
  throw new Error("public Help Center search did not return the enterprise gateway entry");
}
const articleHits = publicApi.searchHelpArticles("restricted company gateway");
if (articleHits[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("public source-cited Help Center search did not rank the restricted gateway article");
}
const streamResult = publicApi.applyAssistantStreamChunk("", "consumer");
if (streamResult.kind !== "replace" || streamResult.text !== "consumer") {
  throw new Error("public stream helper did not apply a consumer update");
}
const broker = new publicApi.GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => new Response(null, { status: 204 }),
});
if (!(broker instanceof publicApi.GrokPtahBrokerClient)) {
  throw new Error("public broker client could not be constructed by a consumer");
}

// The Help core must retrieve, cite, and abstain identically from a consumer.
const helpOutcome = publicApi.searchHelpCorpus("why did my agent send the same request twice after a restart");
if (helpOutcome.results[0]?.articleId !== "operations.durable-recovery") {
  throw new Error("public Help retrieval did not rank the durable-recovery article for a paraphrase");
}
if (helpOutcome.corpusDigest !== publicApi.HELP_CORPUS_DIGEST) {
  throw new Error("public Help retrieval is not bound to the shipped corpus digest");
}
if (!helpOutcome.results[0].citations.every((citation) => citation.path && citation.heading)) {
  throw new Error("public Help retrieval returned a citation without a source anchor");
}
if (!publicApi.searchHelpCorpus("how do I bake sourdough bread").abstained) {
  throw new Error("public Help retrieval answered a question the corpus cannot support");
}
if (!publicApi.verifyHelpModelChecksum().ok) {
  throw new Error("published Help embedding model failed its checksum");
}
const redactedHelp = publicApi.searchHelpCorpus("my key xai-AbCdEf0123456789AbCdEf on the gateway");
if (JSON.stringify(redactedHelp).includes("AbCdEf")) {
  throw new Error("public Help retrieval echoed a credential from the query");
}
if (Object.isFrozen(publicApi.HELP_CORPUS) !== true) {
  throw new Error("published canonical Help corpus must be immutable");
}

const helpReactApi = await import(helpReactBundlePath.href);
for (const name of ["HelpResults", "HelpSearchInput", "HelpCitationList", "HelpHighlightedText", "useHelpSearch", "searchHelpCorpus"]) {
  if (!(name in helpReactApi)) throw new Error(`help-react bundle is missing required export: ${name}`);
}

// A consumer must be able to authorize, and must be denied by default.
const denied = publicApi.authorizeHelpDecision(
  {
    schema: "grokptah.help-authority-request.v1",
    action: "search",
    principal: { principal_id: "p", tenant_id: "t", capabilities: [] },
    corpus_digest: publicApi.HELP_CORPUS_DIGEST,
    index_digest: publicApi.HELP_INDEX_PROVENANCE.indexDigest,
    sources: [],
  },
  publicApi.HELP_CORPUS_DIGEST,
  publicApi.HELP_INDEX_PROVENANCE.indexDigest,
);
if (denied.allowed || denied.denied_because !== "missing_capability") {
  throw new Error("published authority did not deny a principal with no capability");
}
let rejected = false;
try {
  publicApi.parseHelpDecisionRequest({ schema: "x", action: "search", bypass: true });
} catch {
  rejected = true;
}
if (!rejected) throw new Error("published authority accepted an unknown field");

// Claim spans must be re-verifiable by a consumer that did not produce them.
const spanChunk = publicApi.HELP_CORPUS.chunks[0];
const span = publicApi.buildHelpClaimSpan(spanChunk.id, spanChunk.text.slice(0, 16));
if (!span || publicApi.verifyHelpClaimSpan(span).ok !== true) {
  throw new Error("published claim spans did not verify against the published corpus");
}
if (publicApi.verifyHelpClaimSpan({ ...span, startUtf16: span.startUtf16 + 1 }).ok !== false) {
  throw new Error("published claim spans accepted a drifted offset");
}

// The index digest must bind the corpus actually shipped.
if (publicApi.HELP_INDEX_PROVENANCE.corpusDigest !== publicApi.HELP_CORPUS_DIGEST) {
  throw new Error("published index provenance is not bound to the published corpus");
}

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
console.log("help-react entry verified: React externalized, primitives exported");
