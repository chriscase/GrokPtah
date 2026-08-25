/**
 * Disposable ContextDesk-shaped consumer of the published @grokptah/client
 * package. Installed via `npm pack` into a throwaway workspace so imports
 * resolve through node_modules, not the GrokPtah source tree.
 *
 * This is not a live ContextDesk HTTP integration, a Cursor account campaign,
 * or native desktop-authority proof.
 */
import {
  GrokPtahBrokerClient,
  GrokPtahBrokerError,
  parseBrokerApproval,
  parseBrokerBinding,
  parseBrokerErrorEnvelope,
  parseBrokerEventUpdate,
  parseBrokerRun,
  parseBrokerRunProjection,
  parseBrokerReviewProjection,
  EXTERNAL_WORKER_CONTRACT,
  parseExternalWorkerLaunchRequest,
  parseExternalWorkerFollowUpRequest,
  parseExternalWorkerLaunchResult,
  parseExternalWorkerNotification,
  parseExternalWorkerListQuery,
  parseExternalWorkerListPage,
  parseExternalWorkerSummary,
  parseExternalWorkerRecord,
  applyExternalWorkerNotification,
  createExternalWorkerMonitor,
  replaceExternalWorkerMonitor,
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
  parseExternalWorkerListQuery as uiCoreParseListQuery,
  EXTERNAL_WORKER_CONTRACT as UI_CORE_EXTERNAL_WORKER_CONTRACT,
} from "@grokptah/client/ui-core";

function fail(message) {
  throw new Error(message);
}

function jsonResponse(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function sseResponse(text) {
  return new Response(text, {
    status: 200,
    headers: { "content-type": "text/event-stream" },
  });
}

const publicApi = await import("@grokptah/client");
const uiCoreApi = await import("@grokptah/client/ui-core");

if ("GrokPtahClient" in publicApi || "GrokPtahClient" in uiCoreApi) {
  fail("published package leaked the trusted MCP client");
}
if ("GrokPtahBrokerClient" in uiCoreApi) {
  fail("ui-core subpath leaked the browser broker client");
}
if (UI_CORE_EXTERNAL_WORKER_CONTRACT !== EXTERNAL_WORKER_CONTRACT) {
  fail("ui-core external-worker contract drifted from the public package");
}

for (const specifier of [
  "@grokptah/client/trusted",
  "@grokptah/client/src/lib/trusted.ts",
  "grokptah-agent-bridge",
]) {
  const outcome = await import(specifier).then(() => "resolved", () => "rejected");
  if (outcome !== "rejected") fail(`consumer resolved private module ${specifier}`);
}

if (HELP_ARTICLES.length < 1) fail("consumer could not read the Help Center corpus");
if (!Object.isFrozen(HELP_ARTICLES) || !Object.isFrozen(HELP_ENTRIES)) {
  fail("consumer Help corpora were not immutable");
}
if (UI_CORE_HELP_ARTICLES.length !== HELP_ARTICLES.length) {
  fail("ui-core subpath exposed a different Help Center corpus");
}
if (searchHelpArticles("restricted company gateway")[0]?.article?.id !== "providers.restricted-gateway-review") {
  fail("consumer Help Center ranking did not match the published contract");
}
if (uiCoreSearchHelpArticles("restricted company gateway")[0]?.article?.id !== "providers.restricted-gateway-review") {
  fail("ui-core consumer Help Center ranking did not match the published contract");
}
if (applyAssistantStreamChunk("", "consumer").text !== "consumer") {
  fail("consumer stream helper did not apply a bounded update");
}
const queued = promptQueueReducer(emptyPromptQueueState, {
  type: "add",
  sessionId: "consumer-session",
  entry: createPromptQueueEntry("review", { id: "consumer-entry" }),
});
if (queued.entries["consumer-session"]?.[0]?.text !== "review") {
  fail("consumer queue reducer was not usable");
}

if (EXTERNAL_WORKER_CONTRACT !== "grokptah.external-workers.v1") {
  fail("consumer external-worker contract version was not usable");
}

const utf8Prompt = "审查候选用例 caf\u{FFFD}";
const launch = parseExternalWorkerLaunchRequest({
  requestId: "consumer-request",
  provider: "cursor_cloud",
  repository: "org/repo",
  startingRef: "main",
  prompt: utf8Prompt,
  executionMode: "isolated",
  autoCreatePr: false,
});
if (launch?.prompt !== utf8Prompt) fail("consumer launch parser dropped UTF-8 prompt text");
const followUp = parseExternalWorkerFollowUpRequest({
  requestId: "consumer-follow-up",
  prompt: "Re-check the focused candidate",
  bounds: { maxRounds: 8 },
});
if (followUp?.requestId !== "consumer-follow-up") fail("consumer follow-up parser was not usable");
if (parseExternalWorkerLaunchResult({ worker: {}, run: {} }) !== null) {
  fail("consumer launch result parser failed closed");
}

if (parseExternalWorkerListQuery({ limit: 0 }) !== null) {
  fail("consumer list query parser accepted an invalid limit");
}
if (parseExternalWorkerListQuery({ cursor: "page\n2" }) !== null) {
  fail("consumer list query parser accepted a control-character cursor");
}
if (uiCoreParseListQuery({ cursor: "page\n2" }) !== null) {
  fail("ui-core list query parser accepted a control-character cursor");
}
const utf8Query = parseExternalWorkerListQuery({
  limit: 20,
  cursor: "page-审查-1",
  includeArchived: false,
});
if (utf8Query?.cursor !== "page-审查-1") fail("consumer list query parser dropped a UTF-8 cursor");
if (utf8Query?.includeArchived !== false) {
  fail("consumer list query parser dropped explicit includeArchived false");
}
if (parseExternalWorkerListQuery({ includeArchived: null }) !== null) {
  fail("consumer list query parser accepted JSON null includeArchived");
}
if (parseExternalWorkerListQuery({})?.includeArchived !== false) {
  fail("consumer list query parser omitted includeArchived instead of explicit false");
}
if (uiCoreParseListQuery({})?.includeArchived !== false) {
  fail("ui-core list query parser omitted includeArchived instead of explicit false");
}

if (parseExternalWorkerSummary({
  provider: "cursor_cloud",
  externalAgentId: "agent-审查-1",
  repository: "org/repo",
  startingRef: "main",
  state: "ready",
  createdAt: "now",
  updatedAt: "now",
}) !== null) {
  fail("consumer summary parser leaked repository fields");
}
if (parseExternalWorkerSummary({
  provider: "cursor_cloud",
  externalAgentId: "agent-审查-1",
  name: "审查候选用例",
  state: "ready",
  createdAt: "now",
  updatedAt: "now",
}) !== null) {
  fail("consumer summary parser accepted a provider name field");
}

const listPage = parseExternalWorkerListPage({
  items: [{
    provider: "cursor_cloud",
    externalAgentId: "agent-审查-1",
    state: "ready",
    createdAt: "now",
    updatedAt: "now",
  }],
  nextCursor: "page-审查-2",
});
if (listPage?.items[0]?.externalAgentId !== "agent-审查-1") {
  fail("consumer list page parser dropped a UTF-8 worker identity");
}

const archivedWorker = parseExternalWorkerRecord({
  provider: "cursor_cloud",
  externalAgentId: "agent-审查-1",
  repository: "org/repo",
  startingRef: "main",
  state: "archived",
  createdAt: "now",
  updatedAt: "now",
});
if (archivedWorker?.state !== "archived") fail("consumer archive projection parser was not usable");
const restoredWorker = parseExternalWorkerRecord({
  ...archivedWorker,
  state: "ready",
});
if (restoredWorker?.state !== "ready") fail("consumer unarchive projection parser was not usable");

const expiredEnvelope = parseBrokerErrorEnvelope({
  code: "stale_or_recovery",
  message: "列表游标已过期",
  reasonCode: "cursor_expired",
  requestId: "req-审查-1",
  eventRange: { startSeq: 12, endSeq: 18 },
  privilegedPath: "/Users/secret",
});
if (
  expiredEnvelope?.code !== "stale_or_recovery" ||
  expiredEnvelope.reasonCode !== "cursor_expired" ||
  expiredEnvelope.message !== "列表游标已过期" ||
  expiredEnvelope.eventRange?.startSeq !== 12 ||
  "privilegedPath" in expiredEnvelope
) {
  fail("consumer expired-cursor error envelope was not usable");
}
const unknownEnvelope = parseBrokerErrorEnvelope({
  code: "stale_or_recovery",
  message: "list cursor is unknown",
  reasonCode: "unknown_cursor",
});
if (unknownEnvelope?.reasonCode !== "unknown_cursor") {
  fail("consumer unknown-cursor error envelope was not usable");
}
if (parseBrokerErrorEnvelope({
  code: "unauthenticated",
  message: "Authorization: Bearer secret",
}) !== null) {
  fail("consumer error envelope parser exposed a bearer credential");
}
if (parseBrokerBinding({
  bindingId: "binding-1",
  contract: "grokptah.capabilities.v1",
  expiresAt: "2026-08-25T00:00:00Z",
  capabilities: [{ id: "run.review", availability: "available" }],
  token: "secret",
}) !== null) {
  fail("consumer binding parser accepted a credential field");
}
if (parseBrokerRun({ brokerRunId: "run-1", bindingId: "binding-1", workspace: "/secret" }) !== null) {
  fail("consumer run parser accepted a workspace path");
}

const calls = [];
const client = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  token: "sk-should-never-be-sent",
  csrfToken: "csrf-1",
  fetcher: async (input, init) => {
    const url = String(input);
    calls.push({ url, init });
    if ((init?.headers ?? {})["Authorization"]) {
      fail("consumer broker client sent a bearer credential");
    }
    if (url.endsWith("/bindings") && init?.method === "POST") {
      return jsonResponse({
        bindingId: "binding-审查-1",
        contract: "grokptah.capabilities.v1",
        expiresAt: "2026-08-25T23:00:00Z",
        capabilities: [
          { id: "session.observe", availability: "available" },
          { id: "run.execute", availability: "gated" },
          { id: "run.review", availability: "available" },
        ],
      });
    }
    if (url.includes("/external-workers?") && url.includes("includeArchived=false")) {
      return jsonResponse({
        items: [{
          provider: "cursor_cloud",
          externalAgentId: "agent-审查-1",
          state: "ready",
          createdAt: "now",
          updatedAt: "now",
        }],
        nextCursor: "page-审查-2",
      });
    }
    if (url.endsWith("/external-workers/agent-%E5%AE%A1%E6%9F%A5-1/archive")) {
      return jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "agent-审查-1",
        repository: "org/repo",
        startingRef: "main",
        state: "archived",
        createdAt: "now",
        updatedAt: "now",
      });
    }
    if (url.endsWith("/external-workers/agent-%E5%AE%A1%E6%9F%A5-1/unarchive")) {
      return jsonResponse({
        provider: "cursor_cloud",
        externalAgentId: "agent-审查-1",
        repository: "org/repo",
        startingRef: "main",
        state: "ready",
        createdAt: "now",
        updatedAt: "now",
      });
    }
    if (url.includes("/runs/run-1/events")) {
      return sseResponse(
        'id: 4\ndata: {"kind":"event","brokerRunId":"run-1","seq":4,"ts":"now","update":{"type":"progress","detail":"审查"}}\n\n' +
          'data: {"kind":"recovery","brokerRunId":"run-1","afterSeq":4,"reason":"cursor_expired","pollRoute":"/runs/run-1"}\n\n',
      );
    }
    if (url.includes("/runs/run-1") && (init?.method ?? "GET") === "GET") {
      return jsonResponse({
        brokerRunId: "run-1",
        bindingId: "binding-审查-1",
        state: "completed",
        promptPreview: "审查候选用例",
        createdAt: "2026-08-25T00:00:00Z",
        updatedAt: "2026-08-25T00:01:00Z",
      });
    }
    return new Response(null, { status: 204 });
  },
});
if (!(client instanceof GrokPtahBrokerClient)) fail("consumer broker client did not construct");
if ("token" in client) fail("consumer broker client retained a bearer token property");

const binding = await client.createBinding(
  "war-room-42",
  "approved-workspace-alias",
  ["session.observe", "run.execute", "run.review"],
  "bind-1",
);
if (binding.bindingId !== "binding-审查-1") fail("consumer binding did not round-trip an opaque id");
if (!binding.capabilities.every((capability) => ["session.observe", "run.execute", "run.review"].includes(capability.id))) {
  fail("consumer binding was not capability-scoped");
}
if (calls[0]?.init?.headers?.Authorization) fail("binding request leaked an Authorization header");
if (calls[0]?.init?.headers?.["X-CSRF-Token"] !== "csrf-1") fail("binding request omitted broker CSRF");

await client.createBinding("war-room-42", "/Users/secret", ["run.review"], "bind-2").then(
  () => fail("path-like workspace alias was accepted"),
  (error) => {
    if (!(error instanceof GrokPtahBrokerError) || error.code !== "invalid_request") {
      fail("invalid authority workspace did not fail closed");
    }
  },
);

const listed = await client.listExternalWorkers("binding-审查-1", {
  limit: 20,
  cursor: "page-审查-1",
  includeArchived: false,
});
if (listed.items[0]?.externalAgentId !== "agent-审查-1") {
  fail("consumer list did not round-trip a UTF-8 worker identity");
}
if (listed.items[0]?.repository) fail("consumer list leaked a repository field");
if (!String(calls.at(-1)?.url).includes("includeArchived=false")) {
  fail("consumer list omitted the explicit includeArchived flag");
}

const archived = await client.archiveExternalWorker("binding-审查-1", "agent-审查-1", "archive-1");
if (archived.state !== "archived" || archived.externalAgentId !== "agent-审查-1") {
  fail("consumer archive did not return the archived UTF-8 identity");
}
if (!String(calls.at(-1)?.url).includes("/archive")) fail("consumer archive used the wrong route");
if (String(calls.at(-1)?.url).includes("/cancel")) fail("consumer archive implied cancel");

const restored = await client.unarchiveExternalWorker("binding-审查-1", "agent-审查-1", "unarchive-1");
if (restored.state !== "ready") fail("consumer unarchive did not restore an active identity");
if (!String(calls.at(-1)?.url).includes("/unarchive")) fail("consumer unarchive used the wrong route");

const projection = await client.getRunProjection("binding-审查-1", "run-1");
if (projection.state !== "completed" || projection.promptPreview !== "审查候选用例") {
  fail("consumer run projection parser dropped UTF-8 text");
}

const notifications = [];
for await (const notification of client.streamEvents("binding-审查-1", "run-1")) {
  notifications.push(notification);
}
if (notifications.map((notification) => notification.kind).join(",") !== "event,recovery") {
  fail("consumer broker stream did not reconnect through a cursor_expired recovery frame");
}
if (notifications[1]?.reason !== "cursor_expired") {
  fail("consumer broker stream dropped the expiry reason");
}

if (parseBrokerEventUpdate({ type: "progress", detail: "/private/secret" }) !== null) {
  fail("consumer broker event update parser exposed privileged text");
}
if (parseBrokerApproval({
  approvalId: "approval-1",
  bindingId: "binding-1",
  brokerRunId: "run-1",
  sourceFingerprint: "source-1",
  finalFingerprint: "final-1",
  changedFiles: [],
  expiresAt: "2026-08-25T23:00:00Z",
})?.approvalId !== "approval-1") {
  fail("consumer broker approval parser was not usable");
}
if (parseBrokerRunProjection({
  brokerRunId: "run-1",
  bindingId: "binding-1",
  state: "completed",
  promptPreview: "Review",
  createdAt: "2026-08-24T00:00:00Z",
  updatedAt: "2026-08-24T00:01:00Z",
})?.state !== "completed") {
  fail("consumer broker run projection parser was not usable");
}
if (parseBrokerReviewProjection({
  changedFiles: [],
  diff: "diff",
  diffTruncated: false,
  fingerprint: "final-1",
})?.fingerprint !== "final-1") {
  fail("consumer broker review projection parser was not usable");
}

const unauthorized = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => jsonResponse({
    code: "unauthenticated",
    message: "broker session expired",
    reasonCode: "invalid_authority",
  }, 401),
});
await unauthorized.listExternalWorkers("binding-1").then(
  () => fail("invalid authority was accepted"),
  (error) => {
    if (!(error instanceof GrokPtahBrokerError) || error.code !== "unauthenticated" || error.reasonCode !== "invalid_authority") {
      fail("invalid authority did not fail closed with a typed envelope");
    }
  },
);

const expiredList = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => jsonResponse({
    code: "stale_or_recovery",
    message: "resume from the retained window",
    reasonCode: "cursor_expired",
    eventRange: { startSeq: 12, endSeq: 18 },
  }, 410),
});
await expiredList.listExternalWorkers("binding-1", { cursor: "expired-page" }).then(
  () => fail("expired list cursor was accepted"),
  (error) => {
    if (
      !(error instanceof GrokPtahBrokerError) ||
      error.code !== "stale_or_recovery" ||
      error.reasonCode !== "cursor_expired" ||
      error.eventRange?.endSeq !== 18
    ) {
      fail("expired cursor did not fail closed with a typed envelope");
    }
  },
);

const csrfMissing = new GrokPtahBrokerClient({
  baseUrl: "https://contextdesk.example",
  fetcher: async () => jsonResponse({ ok: true }),
});
await csrfMissing.archiveExternalWorker("binding-1", "agent-1", "archive-1").then(
  () => fail("archive without CSRF was accepted"),
  (error) => {
    if (!(error instanceof GrokPtahBrokerError) || error.code !== "csrf_required") {
      fail("archive without CSRF did not fail closed");
    }
  },
);

const monitor = applyExternalWorkerNotification(
  createExternalWorkerMonitor(),
  parseExternalWorkerNotification({
    type: "event",
    event: { seq: 0, ts: "2026-08-25T00:00:00Z", kind: "run.started", detail: "审查" },
  }),
);
if (monitor?.lastSeq !== 0 || monitor.recoveryRequired) {
  fail("consumer external-worker monitor was not usable");
}
const recovered = applyExternalWorkerNotification(monitor, parseExternalWorkerNotification({
  type: "recovery",
  afterSeq: 0,
  reason: "cursor_expired",
  pollRoute: "/runs/run-1",
}));
if (!recovered?.recoveryRequired) fail("consumer monitor did not fence cursor expiry");
const replaced = replaceExternalWorkerMonitor([
  { seq: 0, ts: "now", kind: "run.started", detail: "审查" },
  { seq: 1, ts: "now", kind: "run.progress", detail: "checking" },
]);
if (replaced?.recoveryRequired !== false || replaced.lastSeq !== 1) {
  fail("consumer monitor snapshot did not clear recovery");
}

console.log("context-desk consumer conformance passed");
console.log("external consumer fixture passed");
