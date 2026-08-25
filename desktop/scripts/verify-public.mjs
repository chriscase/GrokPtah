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
  "parseBrokerRunProjection",
  "parseBrokerReviewProjection",
  "searchHelp",
  "searchHelpArticles",
  "HELP_ARTICLES",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
];
const missing = requiredExports.filter((name) => !(name in publicApi));
if (missing.length > 0) {
  throw new Error(`public bundle is missing required exports: ${missing.join(", ")}`);
}
for (const name of ["HELP_ARTICLES", "searchHelpArticles", "promptQueueReducer", "applyAssistantStreamChunk"]) {
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

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
