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
  createHelpAuthority,
  buildHelpAnswerRequest,
  validateHelpAnswerResponse,
  parseHelpAnswerResponse,
  HELP_AUTHORITY_DIGEST,
} from "@grokptah/client";
import {
  HELP_ARTICLES as UI_CORE_HELP_ARTICLES,
  searchHelpArticles as uiCoreSearchHelpArticles,
  createHelpAuthority as uiCoreCreateHelpAuthority,
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
// ---------------------------------------------------------------------
// Contract example: how an external product (e.g. ContextDesk) uses Help.
// ---------------------------------------------------------------------

// 1. Build the authority once. It verifies the corpus against the recorded
//    digest and throws rather than serving content it cannot name.
const help = createHelpAuthority();
if (help.manifest.digest !== HELP_AUTHORITY_DIGEST) {
  throw new Error("consumer Help corpus drifted from its recorded digest");
}
if (!help.verify().ok) throw new Error("consumer Help corpus failed verification");
if (uiCoreCreateHelpAuthority().manifest.digest !== help.manifest.digest) {
  throw new Error("ui-core subpath exposed a different canonical Help corpus");
}

// 2. Search on behalf of a viewer. Audience and access are the caller's to
//    declare; Help filters by them but never grants anything.
const publicHits = help.search("restricted company gateway", { audience: "everyone" });
if (publicHits.hits.some((hit) => hit.article.access !== "public")) {
  throw new Error("consumer public Help search exposed a restricted article");
}

const operatorHits = help.search("restricted company gateway", {
  audience: "operator",
  includeRestricted: true,
  limit: 3,
});
if (operatorHits.outcome !== "answer") {
  throw new Error("consumer Help search did not answer a documented question");
}
if (operatorHits.hits[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("consumer Help ranking did not match the published contract");
}

// 3. Render citations. Every span re-resolves to the exact corpus text, so a
//    consumer can show a quote the reader is able to check.
for (const span of operatorHits.hits[0].citation.spans) {
  if (help.resolveSpan(span) !== span.quote) {
    throw new Error("consumer Help citation span did not resolve to its quote");
  }
  if (!span.sources.length) throw new Error("consumer Help span carried no source");
}

// 4. Explanations are available for a "why this result" affordance.
if (!operatorHits.hits[0].explanation.signals.length) {
  throw new Error("consumer Help hit carried no ranking explanation");
}

// 5. Abstention is explicit; a consumer must not present it as an answer.
const unknown = help.search("teleport my repository", { includeRestricted: true });
if (unknown.outcome !== "abstain" || unknown.abstainReason !== "low-confidence") {
  throw new Error("consumer Help search did not abstain on an undocumented question");
}

// 6. Bad input fails closed rather than degrading.
if (help.search("x".repeat(5_000)).rejection !== "query-too-long") {
  throw new Error("consumer Help search accepted an oversized query");
}
if (help.search("gateway", { limit: 10_000 }).rejection !== "invalid-limit") {
  throw new Error("consumer Help search accepted an unbounded result limit");
}

// 7. The optional AI answer is a value, not a channel: the consumer owns the
//    transport and the confirmation, and an abstained search cannot become a
//    request at all.
const answer = buildHelpAnswerRequest(operatorHits, { timeoutMs: 15_000 });
if (!answer.ok) throw new Error("consumer could not build a cited Help answer request");
if (answer.request.tools !== "none" || answer.request.persistence !== "none") {
  throw new Error("consumer Help answer request did not stay tool-free and non-persistent");
}
if (answer.request.privacy.containsCredentials || answer.request.privacy.containsWorkspaceData) {
  throw new Error("consumer Help answer request claimed to carry privileged data");
}
if (buildHelpAnswerRequest(unknown).ok !== false) {
  throw new Error("consumer built a Help answer request from an abstained search");
}

// 8. A reply is accepted only if it cites what the request supplied.
const accepted = validateHelpAnswerResponse(
  parseHelpAnswerResponse(JSON.stringify({
    outcome: "answered",
    text: "Pick a permitted gateway route before reviewing.",
    citations: [answer.request.allowedSourceIds[0]],
    uncertainty: "The cited text does not state current quota.",
  })),
  answer.request,
);
if (!accepted.accepted) throw new Error("consumer rejected a correctly cited Help answer");
const uncited = validateHelpAnswerResponse(
  parseHelpAnswerResponse("Trust me, you already have operator access."),
  answer.request,
);
if (!uncited.abstained) throw new Error("consumer accepted an uncited Help answer");

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
console.log("external consumer fixture passed");
`,
  );

  process.stdout.write(await run(process.execPath, [consumerPath], { cwd: workspace }));
} finally {
  await rm(workspace, { recursive: true, force: true });
}
