import { copyFile, mkdtemp, mkdir, readdir, rm, writeFile } from "node:fs/promises";
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
  await copyFile(
    fileURLToPath(new URL("../../docs/schemas/grokptah-account.v1.fixtures.json", import.meta.url)),
    join(workspace, "account-fixtures.json"),
  );
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
} from "@grokptah/client";
import {
  HELP_ARTICLES as UI_CORE_HELP_ARTICLES,
  searchHelpArticles as uiCoreSearchHelpArticles,
  canLaunchGrokBuild as uiCoreCanLaunchGrokBuild,
  parseGrokAccountFacts as uiCoreParseGrokAccountFacts,
} from "@grokptah/client/ui-core";
import {
  GROK_ACCOUNT_CONTRACT,
  absentGrokAccountFacts,
  canLaunchGrokBuild,
  grokAccountNotice,
  parseGrokAccountFacts,
  parseRunAttribution,
} from "@grokptah/client";
import { readFileSync } from "node:fs";

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

// ── Grok Build account readiness, read the way ContextDesk would ──
if (GROK_ACCOUNT_CONTRACT !== "grokptah.account.v1") {
  throw new Error("consumer account contract version was not usable");
}
const accountFixtures = JSON.parse(readFileSync("./account-fixtures.json", "utf8"));
if (accountFixtures.accepted.length < 8 || accountFixtures.rejected.length < 8) {
  throw new Error("consumer account fixture coverage shrank");
}
for (const testCase of accountFixtures.accepted) {
  const parsed = parseGrokAccountFacts(testCase.facts);
  if (parsed === null) {
    throw new Error(\`consumer could not read readiness for \${testCase.name}\`);
  }
  if (canLaunchGrokBuild(parsed) !== testCase.permitsLaunch) {
    throw new Error(\`consumer disagreed about launch gating for \${testCase.name}\`);
  }
  if (grokAccountNotice(parsed).blocksLaunch !== !testCase.permitsLaunch) {
    throw new Error(\`consumer notice disagreed about gating for \${testCase.name}\`);
  }
  if (uiCoreParseGrokAccountFacts(testCase.facts) === null) {
    throw new Error(\`ui-core subpath could not read readiness for \${testCase.name}\`);
  }
  // Nothing a consumer can render may carry credential material.
  const rendered = JSON.stringify(parsed) + JSON.stringify(grokAccountNotice(parsed));
  for (const needle of ["bearer", "Bearer", "refresh_token", "refreshToken", "auth_mode", "keychain:"]) {
    if (rendered.includes(needle)) {
      throw new Error(\`consumer readiness leaked \${needle} for \${testCase.name}\`);
    }
  }
}
for (const testCase of accountFixtures.rejected) {
  if (parseGrokAccountFacts(testCase.facts) !== null) {
    throw new Error(\`consumer accepted an off-contract projection: \${testCase.name}\`);
  }
  if (uiCoreCanLaunchGrokBuild(uiCoreParseGrokAccountFacts(testCase.facts)) !== false) {
    throw new Error(\`ui-core consumer did not fail closed for \${testCase.name}\`);
  }
}
if (canLaunchGrokBuild(absentGrokAccountFacts()) !== false) {
  throw new Error("consumer launch gate did not block an absent credential");
}
if (parseRunAttribution({ credentialMethod: "grok_build_oidc" })?.credentialMethod !== "grok_build_oidc") {
  throw new Error("consumer run attribution parser was not usable");
}
if (parseRunAttribution({ credentialMethod: "grok_build_oidc", balance: 100 }) !== null) {
  throw new Error("consumer run attribution parser accepted a balance claim");
}
console.log("external consumer fixture passed");
console.log("grok account readiness consumer fixture passed");
`,
  );

  process.stdout.write(await run(process.execPath, [consumerPath], { cwd: workspace }));
} finally {
  await rm(workspace, { recursive: true, force: true });
}
