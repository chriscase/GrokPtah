import { mkdtemp, mkdir, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";

const packageSource = new URL("../dist/public/", import.meta.url);
const workspace = await mkdtemp(join(tmpdir(), "grokptah-public-consumer-"));
const consumerPath = join(workspace, "consumer.mjs");
const packDirectory = join(workspace, "pack");

function run(command, args, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    child.stdout.on("data", (chunk) => { output += chunk; });
    child.stderr.on("data", (chunk) => { output += chunk; });
    child.on("error", reject);
    child.on("close", (code) => {
      if (code === 0) resolve(output);
      else reject(new Error(`${command} ${args.join(" ")} exited ${code}: ${output}`));
    });
  });
}

try {
  await mkdir(packDirectory, { recursive: true });
  await writeFile(
    join(workspace, "package.json"),
    JSON.stringify({ name: "grokptah-public-consumer-fixture", private: true, type: "module" }, null, 2),
  );
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  await run(npm, ["pack", "--ignore-scripts", "--pack-destination", packDirectory], {
    cwd: fileURLToPath(packageSource),
  });
  const packed = (await readdir(packDirectory)).find((name) => name.endsWith(".tgz"));
  if (!packed) throw new Error("npm pack did not produce a package archive");
  await run(npm, [
    "install",
    "--offline",
    "--ignore-scripts",
    "--no-package-lock",
    "--prefix",
    workspace,
    join(packDirectory, packed),
  ], { cwd: workspace });
  await writeFile(
    consumerPath,
    `import {
  GrokPtahBrokerClient,
  parseBrokerApproval,
  parseBrokerEventUpdate,
  parseBrokerRunProjection,
  parseBrokerReviewProjection,
  EXTERNAL_WORKER_CONTRACT,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerNotification,
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  HELP_ARTICLES,
  HELP_ENTRIES,
  applyAssistantStreamChunk,
  createPromptQueueEntry,
  emptyPromptQueueState,
  promptQueueReducer,
  searchHelpArticles,
  ADAPTIVE_COMPUTER_USE_CONTRACT,
  createAdaptiveController,
  adaptiveIngestObservation,
  adaptiveDecideStep,
  adaptiveCommitPlan,
  adaptiveAuthorizePlan,
  adaptiveStepProjection,
  buildAdaptiveDecisionRequest,
  parseAdaptiveDecisionAnswer,
  parseAdaptiveObservation,
  negotiateAdaptiveCapabilities,
} from "@grokptah/client";
import {
  HELP_ARTICLES as UI_CORE_HELP_ARTICLES,
  searchHelpArticles as uiCoreSearchHelpArticles,
  adaptiveDecideStep as uiCoreAdaptiveDecideStep,
  createAdaptiveController as uiCoreCreateAdaptiveController,
} from "@grokptah/client/ui-core";

if (HELP_ARTICLES.length < 1) throw new Error("consumer could not read the Help Center corpus");
if (!Object.isFrozen(HELP_ARTICLES) || !Object.isFrozen(HELP_ENTRIES)) {
  throw new Error("consumer Help corpora were not immutable");
}
if (UI_CORE_HELP_ARTICLES.length !== HELP_ARTICLES.length) {
  throw new Error("ui-core subpath exposed a different Help Center corpus");
}
if (searchHelpArticles("restricted company gateway")[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("consumer Help Center ranking did not match the published contract");
}
if (uiCoreSearchHelpArticles("restricted company gateway")[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("ui-core consumer Help Center ranking did not match the published contract");
}
if (applyAssistantStreamChunk("", "consumer").text !== "consumer") {
  throw new Error("consumer stream helper did not apply a bounded update");
}
const queued = promptQueueReducer(emptyPromptQueueState, {
  type: "add",
  sessionId: "consumer-session",
  entry: createPromptQueueEntry("review", { id: "consumer-entry" }),
});
if (queued.entries["consumer-session"]?.[0]?.text !== "review") {
  throw new Error("consumer queue reducer was not usable");
}
const broker = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => new Response(null, { status: 204 }),
});
if (!(broker instanceof GrokPtahBrokerClient)) throw new Error("consumer broker client did not construct");
if (parseBrokerApproval({
  approvalId: "approval-1",
  bindingId: "binding-1",
  brokerRunId: "run-1",
  sourceFingerprint: "source-1",
  finalFingerprint: "final-1",
  changedFiles: [],
  expiresAt: "2026-08-24T23:00:00Z",
})?.approvalId !== "approval-1") {
  throw new Error("consumer broker approval parser was not usable");
}
if (parseBrokerRunProjection({
  brokerRunId: "run-1",
  bindingId: "binding-1",
  state: "completed",
  promptPreview: "Review",
  createdAt: "2026-08-24T00:00:00Z",
  updatedAt: "2026-08-24T00:01:00Z",
})?.state !== "completed") {
  throw new Error("consumer broker run projection parser was not usable");
}
if (parseBrokerEventUpdate({ type: "progress", round: 1, maxRounds: 12 })?.type !== "progress") {
  throw new Error("consumer broker event update parser was not usable");
}
if (parseBrokerEventUpdate({ type: "progress", detail: "/private/secret" }) !== null) {
  throw new Error("consumer broker event update parser exposed privileged text");
}
if (parseBrokerReviewProjection({
  changedFiles: [],
  diff: "diff",
  diffTruncated: false,
  fingerprint: "final-1",
})?.fingerprint !== "final-1") {
  throw new Error("consumer broker review projection parser was not usable");
}
if (EXTERNAL_WORKER_CONTRACT !== "grokptah.external-workers.v1") {
  throw new Error("consumer external-worker contract version was not usable");
}
const launch = parseExternalWorkerLaunchRequest({
  requestId: "consumer-request",
  provider: "cursor_cloud",
  repository: "org/repo",
  startingRef: "main",
  prompt: "Review the exact candidate",
  executionMode: "isolated",
  autoCreatePr: false,
});
if (launch?.executionMode !== "isolated") {
  throw new Error("consumer external-worker launch parser was not usable");
}
const followUp = parseExternalWorkerFollowUpRequest({
  requestId: "consumer-follow-up",
  prompt: "Re-check the focused candidate",
  bounds: { maxRounds: 8 },
});
if (followUp?.requestId !== "consumer-follow-up") {
  throw new Error("consumer external-worker follow-up parser was not usable");
}
if (parseExternalWorkerLaunchResult({ worker: {}, run: {} }) !== null) {
  throw new Error("consumer external-worker launch result parser failed closed");
}
const externalNotification = parseExternalWorkerNotification({
  type: "event",
  event: { seq: 0, ts: "2026-08-24T00:00:00Z", kind: "run.started", detail: "started" },
});
const externalState = externalNotification
  ? applyExternalWorkerNotification(createExternalWorkerMonitor(), externalNotification)
  : null;
if (externalState?.lastSeq !== 0 || externalState.recoveryRequired) {
  throw new Error("consumer external-worker monitor was not usable");
}

// An embedding product (ContextDesk and peers) must be able to drive one
// adaptive Computer Use step from the published package alone: no UI
// internals, no transport, no model client.
if (ADAPTIVE_COMPUTER_USE_CONTRACT !== "grokptah.adaptive-computer-use.v1") {
  throw new Error("consumer adaptive Computer Use contract version was not usable");
}
const consumerFrame = "c3d4e5f607182930";
const consumerObservation = parseAdaptiveObservation({
  contract: ADAPTIVE_COMPUTER_USE_CONTRACT,
  runId: "run-consumer",
  observationId: "obs-consumer",
  revision: 1,
  controlEpoch: 2,
  surface: "semantic",
  axAvailable: true,
  domAvailable: false,
  frameDigest: consumerFrame,
  elements: [
    {
      elementId: "button-approve",
      role: "button",
      label: "Approve",
      enabled: true,
      focused: true,
      sensitivity: "normal",
      actionClasses: ["invoke"],
    },
  ],
  elementsTruncated: false,
  contradictions: [],
});
if (!consumerObservation) throw new Error("consumer adaptive observation parser was not usable");
const consumerProjection = {
  runId: "run-consumer",
  ownerSessionId: "session-consumer",
  target: {
    appId: "com.example.reviewer",
    windowId: "window-1",
    generation: 1,
    displayName: "Example Reviewer",
    sensitivity: "normal",
  },
  state: "running",
  controlDisposition: "agent_owned",
  controlEpoch: 2,
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
    grantId: "grant-consumer",
    actionClasses: ["invoke"],
    issuedBy: "operator",
    issuedAt: "2026-08-27T00:00:00Z",
    expiresAt: "2026-08-27T01:00:00Z",
    usesRemaining: 3,
    revoked: false,
    expired: false,
  },
};
const consumerController = createAdaptiveController({
  runId: "run-consumer",
  profile: "balanced",
  budget: { maxSteps: 4 },
});
if (consumerController?.budget?.maxSteps !== 4) {
  throw new Error("consumer adaptive controller did not honour a tightened budget");
}
const consumerIngested = adaptiveIngestObservation(
  consumerController,
  consumerObservation,
  consumerProjection,
);
if (!consumerIngested.ok) throw new Error("consumer adaptive controller rejected a live observation");
const consumerCandidates = [
  {
    candidateId: "cand-approve",
    kind: "invoke",
    elementId: "button-approve",
    actionClass: "invoke",
    mutating: true,
    authorized: true,
    expectation: { kind: "frame_changed" },
  },
];
const consumerDecision = adaptiveDecideStep(consumerIngested.state, consumerCandidates);
if (consumerDecision.kind !== "act" || consumerDecision.modelClass !== "none") {
  throw new Error("consumer adaptive controller did not take the deterministic no-model path");
}
if (!adaptiveAuthorizePlan(consumerIngested.state, consumerDecision.plan).authorized) {
  throw new Error("consumer adaptive plan was not authorized against its own observation");
}
const consumerCommitted = adaptiveCommitPlan(consumerIngested.state, consumerDecision.plan);
if (consumerCommitted?.usage?.steps !== 1 || consumerCommitted.usage.smallModelCalls !== 0) {
  throw new Error("consumer adaptive commit did not charge exactly one model-free step");
}
const stalePlan = { ...consumerDecision.plan, observationRevision: 0 };
if (adaptiveCommitPlan(consumerIngested.state, stalePlan) !== null) {
  throw new Error("consumer adaptive controller let a stale plan mutate");
}
const consumerRequest = buildAdaptiveDecisionRequest(
  consumerIngested.state,
  consumerCandidates,
  "small",
);
if (parseAdaptiveDecisionAnswer("Just click Approve, trust me.", consumerRequest) !== null) {
  throw new Error("consumer adaptive answer parser accepted raw model prose");
}
const consumerStep = JSON.stringify(adaptiveStepProjection(consumerCommitted));
if (consumerStep.includes(consumerFrame) || consumerStep.includes("Approve")) {
  throw new Error("consumer adaptive projection leaked frame or element detail");
}
if (negotiateAdaptiveCapabilities(null).ready !== false) {
  throw new Error("consumer adaptive capability negotiation did not fail closed");
}
const uiCoreAdaptive = uiCoreCreateAdaptiveController({ runId: "run-consumer" });
if (uiCoreAdaptiveDecideStep(uiCoreAdaptive, consumerCandidates).kind !== "abstain") {
  throw new Error("ui-core adaptive controller did not abstain without an observation");
}
console.log("external consumer fixture passed");
`,
  );

  process.stdout.write(await run(process.execPath, [consumerPath], { cwd: workspace }));
} finally {
  await rm(workspace, { recursive: true, force: true });
}
