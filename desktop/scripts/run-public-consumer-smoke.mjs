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
  parseExternalWorkerArtifact,
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
} from "@grokptah/client";
import {
  HELP_ARTICLES as UI_CORE_HELP_ARTICLES,
  searchHelpArticles as uiCoreSearchHelpArticles,
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
if (typeof parseExternalWorkerArtifact !== "function") {
  throw new Error("consumer external-worker artifact parser was not exported");
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
const launchResult = parseExternalWorkerLaunchResult({
  worker: {
    provider: "cursor_cloud",
    externalAgentId: "agent-1",
    repository: "org/repo",
    startingRef: "main",
    state: "running",
    createdAt: "2026-08-24T00:00:00Z",
    updatedAt: "2026-08-24T00:00:00Z",
  },
  run: {
    externalAgentId: "agent-1",
    externalRunId: "run-1",
    state: "running",
    stream: "unsupported",
    lastSeq: null,
    createdAt: "2026-08-24T00:00:00Z",
    updatedAt: "2026-08-24T00:00:00Z",
  },
});
if (launchResult?.run.lastSeq !== null || launchResult.run.stream !== "unsupported") {
  throw new Error("consumer external-worker launch result must not synthesize a stream cursor");
}
if (parseExternalWorkerLaunchResult({ worker: {}, run: {} }) !== null) {
  throw new Error("consumer external-worker launch result parser failed closed");
}
if (parseExternalWorkerLaunchResult({
  worker: launchResult.worker,
  run: { ...launchResult.run, lastSeq: 0 },
}) !== null) {
  throw new Error("consumer external-worker parser accepted a fake lastSeq cursor");
}
if (parseExternalWorkerLaunchRequest({ ...launch, repository: "org/repo\\n" }) !== null) {
  throw new Error("consumer external-worker parser accepted a newline repository");
}
if (parseExternalWorkerArtifact({
  path: "artifacts/handoff.md",
  digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  runId: "run-1",
})?.runId !== "run-1") {
  throw new Error("consumer external-worker artifact parser was not usable");
}
if (parseExternalWorkerArtifact({
  path: "artifacts/handoff.md",
  digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
}) !== null) {
  throw new Error("consumer external-worker artifact parser accepted a missing runId");
}
if (parseExternalWorkerArtifact({
  path: "artifacts/handoff.md",
  digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  runId: "run-1",
  url: "https://secret.example/file",
}) !== null) {
  throw new Error("consumer external-worker artifact parser accepted a raw URL");
}
const worker = launchResult.worker;
const run = launchResult.run;
const artifact = { path: "artifacts/handoff.md", digest: "sha256:abc", runId: "run-1" };
const mutatingBroker = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  csrfToken: "csrf-1",
  fetcher: async (input, init) => {
    const url = String(input);
    const method = init?.method ?? "GET";
    let body = null;
    if (method === "POST" && url.endsWith("/external-workers")) body = { worker, run };
    else if (method === "GET" && url.endsWith("/external-workers/agent-1")) body = worker;
    else if (method === "POST" && url.endsWith("/external-workers/agent-1/runs")) body = run;
    else if (method === "POST" && url.endsWith("/runs/run-1/cancel")) body = { ...run, state: "cancelled" };
    else if (method === "GET" && url.endsWith("/runs/run-1/artifacts")) body = [artifact];
    if (!body) return new Response(null, { status: 404 });
    return new Response(JSON.stringify(body), {
      status: 200,
      headers: { "content-type": "application/json" },
    });
  },
});
if ((await mutatingBroker.launchExternalWorker("binding-1", launch, "consumer-request")).run.lastSeq !== null) {
  throw new Error("consumer launch client synthesized a stream cursor");
}
if ((await mutatingBroker.followUpExternalWorker("binding-1", "agent-1", followUp, "consumer-follow-up")).externalRunId !== "run-1") {
  throw new Error("consumer follow-up client was not usable");
}
if ((await mutatingBroker.cancelExternalWorker("binding-1", "agent-1", "run-1", "consumer-cancel")).state !== "cancelled") {
  throw new Error("consumer cancel client was not terminal");
}
if ((await mutatingBroker.getExternalWorkerArtifacts("binding-1", "agent-1", "run-1"))[0]?.runId !== "run-1") {
  throw new Error("consumer artifacts client was not run-attributed");
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
