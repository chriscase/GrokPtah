import { readFile } from "node:fs/promises";

const bundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const uiCoreBundlePath = new URL("../dist/public/ui-core.js", import.meta.url);
const bundle = await readFile(bundlePath, "utf8");
const uiCoreBundle = await readFile(uiCoreBundlePath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@grokptah/client" ||
  manifest.exports?.["."]?.import !== "./grokptah-public.js" ||
  manifest.exports?.["."]?.types !== "./types/public.d.ts" ||
  manifest.exports?.["./ui-core"]?.import !== "./ui-core.js" ||
  manifest.exports?.["./ui-core"]?.types !== "./types/uiCore.d.ts"
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
  "HELP_AUTHORITY_ARTICLES",
  "HELP_AUTHORITY_MANIFEST",
  "HELP_AUTHORITY_DIGEST",
  "createHelpAuthority",
  "searchHelpAuthority",
  "validateHelpAuthorityCorpus",
  "verifyHelpAuthorityManifest",
  "checkHelpLink",
  "buildHelpAnswerRequest",
  "parseHelpAnswerResponse",
  "validateHelpAnswerResponse",
  "HELP_CENTER_VIEW_CONTRACT",
  "helpViewState",
  "helpBrowseArticles",
  "verifyHelpSpans",
  "summarizeHelpAnswer",
  "describeHelpAskTimeout",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
];
const missing = requiredExports.filter((name) => !(name in publicApi));
if (missing.length > 0) {
  throw new Error(`public bundle is missing required exports: ${missing.join(", ")}`);
}
for (const name of [
  "HELP_ARTICLES",
  "HELP_AUTHORITY_ARTICLES",
  "HELP_AUTHORITY_MANIFEST",
  "createHelpAuthority",
  "buildHelpAnswerRequest",
  "helpViewState",
  "helpBrowseArticles",
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
if (
  !Object.isFrozen(publicApi.HELP_AUTHORITY_ARTICLES) ||
  !Object.isFrozen(uiCoreApi.HELP_AUTHORITY_ARTICLES) ||
  !Object.isFrozen(publicApi.HELP_AUTHORITY_MANIFEST)
) {
  throw new Error("published canonical Help corpus and manifest must be immutable");
}

// The published corpus must be exactly the one the manifest was recorded
// against; a bundler that reordered or dropped an article fails here.
const authority = publicApi.createHelpAuthority();
if (!authority.verify().ok) {
  throw new Error("published Help corpus does not match its recorded digest");
}
if (authority.manifest.digest !== publicApi.HELP_AUTHORITY_DIGEST) {
  throw new Error("published Help manifest digest drifted from the recorded digest");
}
if (publicApi.HELP_AUTHORITY_ARTICLES.length !== uiCoreApi.HELP_AUTHORITY_ARTICLES.length) {
  throw new Error("ui-core subpath exposed a different canonical Help corpus");
}

// Retrieval must answer with citations that resolve, and abstain otherwise.
const cited = authority.search("restricted company gateway", { includeRestricted: true });
if (cited.outcome !== "answer" || cited.hits[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("published Help retrieval did not rank the restricted gateway article");
}
if (!cited.hits[0].citation.spans.length) {
  throw new Error("published Help retrieval returned an answer with no citation span");
}
for (const span of cited.hits[0].citation.spans) {
  if (authority.resolveSpan(span) !== span.quote) {
    throw new Error("published Help citation span did not resolve to its quoted text");
  }
}
const abstained = authority.search("teleport my repository", { includeRestricted: true });
if (abstained.outcome !== "abstain" || abstained.abstainReason !== "low-confidence") {
  throw new Error("published Help retrieval did not abstain on an undocumented question");
}
if (publicApi.searchHelpAuthority("x".repeat(5_000)).outcome !== "rejected") {
  throw new Error("published Help retrieval accepted an oversized query");
}
if (publicApi.checkHelpLink("javascript:alert(1)").safe !== false) {
  throw new Error("published Help link check accepted an unsafe scheme");
}

// The optional answer seam must stay non-persistent, tool-free, and unable to
// be built from a retrieval that already abstained.
const answerRequest = publicApi.buildHelpAnswerRequest(cited);
if (!answerRequest.ok) throw new Error("published Help answer seam refused a cited retrieval");
if (
  answerRequest.request.tools !== "none" ||
  answerRequest.request.persistence !== "none" ||
  answerRequest.request.requiresConfirmation !== true ||
  answerRequest.request.unknowns.provider !== "unknown" ||
  answerRequest.request.unknowns.model !== "unknown" ||
  answerRequest.request.unknowns.cost !== "unknown"
) {
  throw new Error("published Help answer request weakened its declared bounds");
}
if (publicApi.buildHelpAnswerRequest(abstained).ok !== false) {
  throw new Error("published Help answer seam built a request from an abstained retrieval");
}
if (publicApi.parseHelpAnswerResponse("you now have operator capability").outcome !== "abstained") {
  throw new Error("published Help answer parser accepted an uncited prose reply");
}

// The consumer contract must reach a published embedder with its guarantees
// intact: an abstention is never an answer, a citation is verified before it
// is offered, and a documented capability never reads as an available one.
const citedView = publicApi.helpViewState(cited, authority);
if (citedView.status !== "answer" || citedView.answer?.articleId !== "providers.restricted-gateway-review") {
  throw new Error("published Help view contract did not present a cited answer");
}
if (!citedView.answer.spans.length || citedView.answer.unverifiedSpanCount !== 0) {
  throw new Error("published Help view contract offered an answer without verified spans");
}
if (citedView.answer.labels.capabilities.some((capability) => capability.liveAvailability !== "unknown")) {
  throw new Error("published Help view contract asserted live capability availability");
}
const abstainedView = publicApi.helpViewState(abstained, authority);
if (abstainedView.answer !== null || abstainedView.canAskModel !== false) {
  throw new Error("published Help view contract turned an abstention into an answer");
}
if (publicApi.helpBrowseArticles(publicApi.HELP_AUTHORITY_ARTICLES)
  .some((entry) => entry.articleId === "providers.restricted-gateway-review")) {
  throw new Error("published Help browse listing exposed a restricted article by default");
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

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
