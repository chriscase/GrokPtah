import { readFile } from "node:fs/promises";

const bundlePath = new URL("../dist/public/grokptah-public.js", import.meta.url);
const bundle = await readFile(bundlePath, "utf8");
const manifest = JSON.parse(
  await readFile(new URL("../dist/public/package.json", import.meta.url), "utf8"),
);
if (
  manifest.name !== "@grokptah/client" ||
  manifest.exports?.["."]?.import !== "./grokptah-public.js" ||
  manifest.exports?.["."]?.types !== "./types/public.d.ts"
) {
  throw new Error("public package manifest does not expose the expected safe entry point");
}
const forbidden = ["@tauri-apps", "trusted.ts", "Authorization: Bearer"];
const leaked = forbidden.filter((needle) => bundle.includes(needle));
if (leaked.length > 0) {
  throw new Error(`public bundle contains forbidden authority markers: ${leaked.join(", ")}`);
}

const publicApi = await import(bundlePath.href);
const requiredExports = [
  "GrokPtahBrokerClient",
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
