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
  "promptQueueReducer",
  "applyAssistantStreamChunk",
  "ADAPTIVE_COMPUTER_USE_CONTRACT",
  "createAdaptiveController",
  "adaptiveIngestObservation",
  "adaptiveDecideStep",
  "buildAdaptiveDecisionRequest",
  "parseAdaptiveDecisionAnswer",
  "adaptiveAdoptModelDecision",
  "adaptiveAuthorizePlan",
  "adaptiveCommitPlan",
  "adaptiveVerifyPlan",
  "adaptiveRecordVerification",
  "adaptiveStepProjection",
  "negotiateAdaptiveCapabilities",
  "parseAdaptiveObservation",
  "parseAdaptiveCandidate",
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
  "ADAPTIVE_COMPUTER_USE_CONTRACT",
  "createAdaptiveController",
  "adaptiveDecideStep",
  "adaptiveStepProjection",
  "parseAdaptiveDecisionAnswer",
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
// The adaptive Computer Use controller is a published boundary: verify in the
// built bundle that it decides deterministically without a model, refuses
// off-grammar gateway output, and projects nothing privileged.
if (publicApi.ADAPTIVE_COMPUTER_USE_CONTRACT !== "grokptah.adaptive-computer-use.v1") {
  throw new Error("public adaptive Computer Use contract version changed unexpectedly");
}
const adaptiveFrame = "a1b2c3d4e5f60718";
const adaptiveObservation = publicApi.parseAdaptiveObservation({
  contract: publicApi.ADAPTIVE_COMPUTER_USE_CONTRACT,
  runId: "run-verify",
  observationId: "obs-verify",
  revision: 1,
  controlEpoch: 4,
  surface: "semantic",
  axAvailable: true,
  domAvailable: false,
  frameDigest: adaptiveFrame,
  elements: [
    {
      elementId: "button-save",
      role: "button",
      label: "Save",
      enabled: true,
      focused: true,
      sensitivity: "normal",
      actionClasses: ["invoke"],
    },
  ],
  elementsTruncated: false,
  contradictions: [],
});
if (!adaptiveObservation) throw new Error("public adaptive observation parser rejected a valid observation");
if (publicApi.parseAdaptiveObservation({ ...adaptiveObservation, frameDigest: "not-a-digest" }) !== null) {
  throw new Error("public adaptive observation parser accepted a non-opaque frame digest");
}
if (publicApi.parseAdaptiveCandidate({
  candidateId: "cand-exec",
  kind: "execute",
  elementId: "button-save",
  actionClass: "invoke",
  mutating: true,
  authorized: true,
  expectation: { kind: "frame_changed" },
}) !== null) {
  throw new Error("public adaptive candidate parser accepted a generic execute escape");
}
const adaptiveController = publicApi.createAdaptiveController({
  runId: "run-verify",
  profile: "balanced",
});
if (!adaptiveController) throw new Error("public adaptive controller could not be constructed");
const adaptiveIngested = publicApi.adaptiveIngestObservation(adaptiveController, adaptiveObservation, {
  runId: "run-verify",
  ownerSessionId: "session-verify",
  target: {
    appId: "com.example.editor",
    windowId: "window-1",
    generation: 1,
    displayName: "Example Editor",
    sensitivity: "normal",
  },
  state: "running",
  controlDisposition: "agent_owned",
  controlEpoch: 4,
  version: 1,
  agentActive: true,
  terminal: false,
  createdAt: "2026-08-27T00:00:00Z",
  updatedAt: "2026-08-27T00:00:01Z",
  progress: {
    actionCount: 0,
    maxActions: 10,
    evidenceBytes: 0,
    maxEvidenceBytes: 1024,
    maxDurationSecs: 60,
    durationExceeded: false,
  },
  grant: {
    grantId: "grant-verify",
    actionClasses: ["invoke"],
    issuedBy: "operator",
    issuedAt: "2026-08-27T00:00:00Z",
    expiresAt: "2026-08-27T01:00:00Z",
    usesRemaining: 5,
    revoked: false,
    expired: false,
  },
});
if (!adaptiveIngested.ok) throw new Error("public adaptive controller rejected an authorized observation");
const adaptiveCandidates = [
  {
    candidateId: "cand-save",
    kind: "invoke",
    elementId: "button-save",
    actionClass: "invoke",
    mutating: true,
    authorized: true,
    expectation: { kind: "frame_changed" },
  },
];
const adaptiveDecision = publicApi.adaptiveDecideStep(adaptiveIngested.state, adaptiveCandidates);
if (adaptiveDecision.kind !== "act" || adaptiveDecision.modelClass !== "none") {
  throw new Error("public adaptive controller did not take the deterministic no-model path");
}
const adaptiveRequest = publicApi.buildAdaptiveDecisionRequest(
  adaptiveIngested.state,
  adaptiveCandidates,
  "small",
);
if (!adaptiveRequest?.grammar?.gbnf?.includes('candidate ::= "\\"cand-save\\""')) {
  throw new Error("public adaptive decision request did not carry a candidate-constrained grammar");
}
if (publicApi.parseAdaptiveDecisionAnswer("I think you should click Save.", adaptiveRequest) !== null) {
  throw new Error("public adaptive answer parser accepted raw model prose");
}
if (publicApi.parseAdaptiveDecisionAnswer(
  '{"candidateId":"cand-save","confidence":0.9,"rationaleCode":"matches_goal_semantics","abstain":false,"reasoning":"free text"}',
  adaptiveRequest,
) !== null) {
  throw new Error("public adaptive answer parser accepted an off-grammar free-text field");
}
const adaptiveProjection = JSON.stringify(publicApi.adaptiveStepProjection(adaptiveIngested.state));
if (adaptiveProjection.includes(adaptiveFrame) || adaptiveProjection.includes("Save")) {
  throw new Error("public adaptive projection leaked frame or element detail");
}

const broker = new publicApi.GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => new Response(null, { status: 204 }),
});
if (!(broker instanceof publicApi.GrokPtahBrokerClient)) {
  throw new Error("public broker client could not be constructed by a consumer");
}

console.log(`public bundle verified: ${requiredExports.join(", ")}`);
