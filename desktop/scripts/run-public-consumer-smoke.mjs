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
  HELP_CORPUS_VERSION,
  applyAssistantStreamChunk,
  createPromptQueueEntry,
  emptyPromptQueueState,
  promptQueueReducer,
  searchHelp,
  searchHelpArticles,
} from "@grokptah/client";
import {
  HELP_ARTICLES as UI_CORE_HELP_ARTICLES,
  searchHelp as uiCoreSearchHelp,
  searchHelpArticles as uiCoreSearchHelpArticles,
} from "@grokptah/client/ui-core";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";

if (HELP_ARTICLES.length < 1) throw new Error("consumer could not read the Help Center corpus");
if (!Object.isFrozen(HELP_ARTICLES)) {
  throw new Error("consumer Help corpus was not immutable");
}
if (HELP_CORPUS_VERSION !== "product-corpus-v1") {
  throw new Error("consumer did not receive the live source-cited Help corpus");
}
if (UI_CORE_HELP_ARTICLES !== HELP_ARTICLES) {
  throw new Error("ui-core subpath exposed a different Help Center corpus");
}
// A consumer reaching for the obvious name must land on the corpus the desktop
// displays — not the access-gated grokptah.help.v1 corpus.
if (searchHelp !== searchHelpArticles || uiCoreSearchHelp !== uiCoreSearchHelpArticles) {
  throw new Error("consumer Help search names resolved to different corpora");
}
if (searchHelpArticles("restricted company gateway")[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("consumer Help Center ranking did not match the published contract");
}
if (uiCoreSearchHelpArticles("restricted company gateway")[0]?.article?.id !== "providers.restricted-gateway-review") {
  throw new Error("ui-core consumer Help Center ranking did not match the published contract");
}
// The shared visual layer must resolve through normal package resolution, and
// must stay a token contract rather than a component library.
const requireFromConsumer = createRequire(import.meta.url);
const tokensPath = requireFromConsumer.resolve("@grokptah/client/styles/tokens.css");
const tokens = readFileSync(tokensPath, "utf8");
for (const required of ['[data-theme="light"]', "--fs-11:", "--type-scale:", ".sr-only", ":focus-visible"]) {
  if (!tokens.includes(required)) {
    throw new Error("consumer stylesheet is missing the published contract piece: " + required);
  }
}
for (const componentish of [".permission-", ".computer-", ".help-", ".modal"]) {
  if (tokens.includes(componentish)) {
    throw new Error("consumer stylesheet exported a component rule: " + componentish);
  }
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
console.log("external consumer fixture passed (contracts + styles/tokens.css)");
`,
  );

  process.stdout.write(await run(process.execPath, [consumerPath], { cwd: workspace }));
} finally {
  await rm(workspace, { recursive: true, force: true });
}
