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
  "HELP_PUBLIC_CORPUS",
  "HELP_PUBLIC_CORPUS_DIGEST",
  "searchHelpCorpus",
  "verifyHelpCorpus",
  "verifyHelpProjection",
  "assertPublicOnly",
  "promptQueueReducer",
  "applyAssistantStreamChunk",
];
const missing = requiredExports.filter((name) => !(name in publicApi));
if (missing.length > 0) {
  throw new Error(`public bundle is missing required exports: ${missing.join(", ")}`);
}
for (const name of [
  "HELP_PUBLIC_CORPUS",
  "searchHelpCorpus",
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
// Semantic Help must ship its offline half and nothing else. These names are
// the authority constructors, the executor, and the transport: a bundle that
// exports any of them lets a consumer decide, in code it controls, what it is
// allowed to see, or point GrokPtah's contract at an endpoint of its choosing.
const forbiddenHelpExports = [
  "issueGrant",
  "authorizeHelpDecision",
  "authorizeHelpDecisionJson",
  "parseHelpDecisionRequest",
  "buildHelpManifest",
  "createHelpGrant",
  "createHelpAdmission",
  "createHelpExecutor",
  "HelpExecutor",
  "runHelpTask",
  "helpAsk",
  "helpFollow",
  "helpCancel",
  "helpBounds",
  "helpSession",
  "helpVisibleCorpus",
  "requestHelpAnswer",
  "HelpAnswerTransport",
  "selectHelpRoute",
  "invoke",
];
for (const [label, api] of [["public", publicApi], ["ui-core", uiCoreApi]]) {
  const exported = forbiddenHelpExports.filter((name) => name in api);
  if (exported.length > 0) {
    throw new Error(
      `${label} bundle exports Help authority or transport: ${exported.join(", ")}`,
    );
  }
}

// The published corpus must be public-only, and must not merely hide the rest
// behind a filtered index: a bundle that carries restricted text has leaked it.
publicApi.assertPublicOnly(publicApi.HELP_PUBLIC_CORPUS);
publicApi.verifyHelpCorpus(publicApi.HELP_PUBLIC_CORPUS);
for (const record of [
  ...publicApi.HELP_PUBLIC_CORPUS.sources,
  ...publicApi.HELP_PUBLIC_CORPUS.articles,
  ...publicApi.HELP_PUBLIC_CORPUS.chunks,
]) {
  if (record.visibility !== "public") {
    throw new Error(`published Help bundle carries a non-public record: ${record.id}`);
  }
}

// The restricted text must be absent from the emitted bytes, not merely
// unexported. This is the check that matters: an earlier version of the public
// surface imported its verifier from a module that loaded the full corpus at
// the top level, so all 27 restricted chunks were bundled while every
// export-level assertion still passed. An export list does not describe what a
// bundler emits.
const privateCorpus = JSON.parse(
  await readFile(new URL("../src/lib/help/canonical/help-corpus.v1.json", import.meta.url), "utf8"),
);
const restricted = [
  ...privateCorpus.chunks.filter((chunk) => chunk.visibility !== "public"),
  ...privateCorpus.articles.filter((article) => article.visibility !== "public"),
];
if (restricted.length === 0) {
  throw new Error("the corpus has no restricted records, so this check proves nothing");
}
for (const [label, text] of [["public", bundle], ["ui-core", uiCoreBundle]]) {
  const leaked = restricted
    .filter((record) => text.includes((record.text ?? record.summary).slice(0, 48)))
    .map((record) => record.id);
  if (leaked.length > 0) {
    throw new Error(
      `${label} bundle carries ${leaked.length} restricted Help record(s): ${leaked
        .slice(0, 5)
        .join(", ")}`,
    );
  }
}

const helpOutcome = publicApi.searchHelpCorpus("recover an interrupted run");
if (helpOutcome.kind !== "results" || helpOutcome.results[0]?.articleId !== "operations.durable-recovery") {
  throw new Error("public Help retrieval did not rank the durable recovery article");
}
// Abstention is part of the shipped behaviour, not an accident of ranking.
if (publicApi.searchHelpCorpus("what is the capital of Portugal").kind !== "abstained") {
  throw new Error("public Help retrieval answered a question the corpus cannot answer");
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
