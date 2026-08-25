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
// The account contract publishes readiness, never credential material. These
// needles would only appear if a field or literal leaked into the projection.
const accountForbidden = ["refresh_token", "refreshToken", "auth_mode", "keychain:", "XAI_TOKEN_AUTH"];
for (const [label, text] of [["public", bundle], ["ui-core", uiCoreBundle]]) {
  const leakedAccount = accountForbidden.filter((needle) => text.includes(needle));
  if (leakedAccount.length > 0) {
    throw new Error(`${label} bundle leaks account credential markers: ${leakedAccount.join(", ")}`);
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
  "GROK_ACCOUNT_CONTRACT",
  "parseGrokAccountFacts",
  "parseRunAttribution",
  "canLaunchGrokBuild",
  "grokAccountNotice",
  "absentGrokAccountFacts",
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
  "GROK_ACCOUNT_CONTRACT",
  "parseGrokAccountFacts",
  "canLaunchGrokBuild",
  "grokAccountNotice",
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
// Readiness must be readable, and must fail closed on an off-contract payload.
const readyFacts = {
  contract: "grokptah.account.v1",
  schemaVersion: 1,
  credentialMethod: "grok_build_oidc",
  accountReference: { value: "usr-0a1b2c3d", source: "user_id" },
  expiry: { status: "valid", expiresAt: "2026-08-25T12:30:00Z", secondsRemaining: 45000 },
  readiness: "usable",
  readinessReason: "expiry_in_future",
};
if (publicApi.parseGrokAccountFacts(readyFacts)?.readiness !== "usable") {
  throw new Error("public account facts parser did not accept a valid projection");
}
if (publicApi.parseGrokAccountFacts({ ...readyFacts, bearer: "xai-secret" }) !== null) {
  throw new Error("public account facts parser accepted a credential-bearing payload");
}
if (publicApi.canLaunchGrokBuild(publicApi.absentGrokAccountFacts()) !== false) {
  throw new Error("public launch gate did not block an absent credential");
}
if (publicApi.grokAccountNotice(readyFacts).blocksLaunch !== false) {
  throw new Error("public readiness notice blocked a valid credential");
}

const broker = new publicApi.GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => new Response(null, { status: 204 }),
});
if (!(broker instanceof publicApi.GrokPtahBrokerClient)) {
  throw new Error("public broker client could not be constructed by a consumer");
}

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
