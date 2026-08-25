import { readFile } from "node:fs/promises";

const bundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const uiCoreBundlePath = new URL("../dist/public/ui-core.js", import.meta.url);
const tokensCssPath = new URL("../dist/public/styles/tokens.css", import.meta.url);
const bundle = await readFile(bundlePath, "utf8");
const uiCoreBundle = await readFile(uiCoreBundlePath, "utf8");
const tokensCss = await readFile(tokensCssPath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@grokptah/client" ||
  manifest.exports?.["."]?.import !== "./grokptah-public.js" ||
  manifest.exports?.["."]?.types !== "./types/public.d.ts" ||
  manifest.exports?.["./ui-core"]?.import !== "./ui-core.js" ||
  manifest.exports?.["./ui-core"]?.types !== "./types/uiCore.d.ts" ||
  manifest.exports?.["./styles/tokens.css"] !== "./styles/tokens.css" ||
  !manifest.files?.includes("styles")
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

// The shared visual layer is a token/a11y contract, not a component library.
// Anything component- or authority-shaped in it would make the continuity
// claim wider than what actually ships.
const leakedCss = forbidden.filter((needle) => tokensCss.includes(needle));
if (leakedCss.length > 0) {
  throw new Error(`tokens.css contains forbidden authority markers: ${leakedCss.join(", ")}`);
}
for (const required of [
  '[data-theme="light"]',
  "--accent-label:",
  "--fs-11:",
  "--density-root:",
  "--type-scale:",
  ":focus-visible",
  ".sr-only",
  "@media (prefers-contrast: more)",
  "@media (forced-colors: active)",
]) {
  if (!tokensCss.includes(required)) {
    throw new Error(`tokens.css is missing the published contract piece: ${required}`);
  }
}
for (const componentish of [".permission-", ".computer-", ".help-", ".composer-", ".modal"]) {
  if (tokensCss.includes(componentish)) {
    throw new Error(`tokens.css exports a component rule (${componentish}); the shared layer is tokens only`);
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
  "HELP_CORPUS_VERSION",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
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

// Exactly one Help corpus reaches browser consumers, and both names resolve to
// it. The access-gated grokptah.help.v1 corpus stays behind `trusted.ts`.
for (const api of [publicApi, uiCoreApi]) {
  if (api.searchHelp !== api.searchHelpArticles) {
    throw new Error("public Help search names resolve to different corpora");
  }
  if (api.HELP_ARTICLES !== publicApi.HELP_ARTICLES) {
    throw new Error("public entry points expose different Help corpora");
  }
  if (api.HELP_CORPUS_VERSION !== "product-corpus-v1") {
    throw new Error("public Help corpus is not the live source-cited corpus");
  }
}
for (const gated of ["HELP_ENTRIES", "HELP_CONTRACT", "buildHelpAssistantContext"]) {
  if (gated in publicApi || gated in uiCoreApi) {
    throw new Error(`public bundle still exposes the access-gated Help corpus: ${gated}`);
  }
}
const articleHits = publicApi.searchHelpArticles("restricted company gateway");
if (articleHits[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("public source-cited Help Center search did not rank the restricted gateway article");
}
if (publicApi.searchHelp("restricted company gateway")[0]?.article?.id !== articleHits[0]?.article?.id) {
  throw new Error("searchHelp and searchHelpArticles ranked different corpora");
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
console.log(`public stylesheet verified: styles/tokens.css (${tokensCss.length} bytes)`);
