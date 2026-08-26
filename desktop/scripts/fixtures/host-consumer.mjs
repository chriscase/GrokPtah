/**
 * External trusted-host consumer fixture.
 *
 * Stands in for a ContextDesk-class desktop/server product: it installs the
 * published `@grokptah/client` package and drives GrokPtah's reusable powers
 * through the `@grokptah/client/host` seam only, against a synthetic in-process
 * transport. No network, no credentials, no live service.
 */
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

import {
  CAPABILITY_CONTRACT,
  GROKPTAH_HOST_CONTRACT,
  GROKPTAH_MAX_ROUNDS,
  GROKPTAH_RECOVERY_POLL_TOOL,
  GrokPtahCapabilityError,
  GrokPtahHost,
  GrokPtahScopeError,
  applyGrokPtahRunNotification,
  assertGrokPtahScope,
  createGrokPtahRunMonitor,
  negotiateGrokPtahCapabilities,
  parseCapabilitySet,
  parseGrokPtahRunScope,
  parseGrokPtahScope,
  validateGrokPtahBounds,
} from "@grokptah/client/host";

const SCOPE = { sessionId: "consumer-session", workspace: "consumer-workspace" };
const RUN_SCOPE = { ...SCOPE, runId: "consumer-run" };

const CAPABILITIES = [
  capability("session.observe", "observe", false, false, "available"),
  capability("run.review", "review", false, false, "available"),
  capability("run.execute", "execute", true, false, "available"),
  capability("run.queue", "execute", true, false, "available"),
  capability("agent.continuity", "observe", false, false, "available"),
  capability("agent.resume", "execute", true, false, "available"),
  capability("run.promote", "promote", true, true, "gated"),
];

function capability(id, tier, mutating, humanGate, availability) {
  return {
    id,
    tier,
    mutating,
    human_gate: humanGate,
    availability,
    description: `synthetic ${id}`,
  };
}

function sseFrame(id, payload) {
  const prefix = id === null ? "" : `id: ${id}\n`;
  return `${prefix}data: ${JSON.stringify(payload)}\n\n`;
}

function eventFrame(seq, detail) {
  return sseFrame(seq, {
    jsonrpc: "2.0",
    method: "notifications/ptah_event",
    params: {
      sessionId: RUN_SCOPE.sessionId,
      workspace: RUN_SCOPE.workspace,
      runId: RUN_SCOPE.runId,
      seq,
      ts: "2026-08-24T00:00:00Z",
      update: { detail },
    },
  });
}

function recoveryFrame(afterSeq, pollTool) {
  return sseFrame(null, {
    jsonrpc: "2.0",
    method: "notifications/ptah_recovery",
    params: {
      sessionId: RUN_SCOPE.sessionId,
      workspace: RUN_SCOPE.workspace,
      runId: RUN_SCOPE.runId,
      afterSeq,
      reason: "cursor_gap",
      pollTool,
    },
  });
}

/** A synthetic MCP endpoint: no sockets, no credentials, no live service. */
function createTransport({ capabilities = CAPABILITIES, frames = [] } = {}) {
  const calls = [];
  const fetcher = async (input, init = {}) => {
    const method = init.method ?? "GET";
    const url = input instanceof URL ? input : new URL(String(input));
    if (method === "GET") {
      calls.push({ kind: "stream", search: Object.fromEntries(url.searchParams) });
      const encoder = new TextEncoder();
      const body = new ReadableStream({
        start(controller) {
          for (const frame of frames) controller.enqueue(encoder.encode(frame));
          controller.close();
        },
      });
      return new Response(body, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      });
    }
    if (method === "DELETE") {
      calls.push({ kind: "close" });
      return new Response(null, { status: 204 });
    }
    const request = JSON.parse(init.body);
    const headers = { "content-type": "application/json", "mcp-session-id": "consumer-transport" };
    if (request.method === "initialize") {
      calls.push({ kind: "initialize" });
      return new Response(
        JSON.stringify({
          jsonrpc: "2.0",
          id: request.id,
          result: {
            protocolVersion: "2025-03-26",
            serverInfo: {
              name: "grokptah-synthetic",
              version: "0.0.0",
              capabilityContract: { contract: CAPABILITY_CONTRACT, capabilities },
            },
          },
        }),
        { status: 200, headers },
      );
    }
    if (request.method === "notifications/initialized") {
      return new Response(null, { status: 202, headers });
    }
    if (request.method === "tools/call") {
      calls.push({ kind: "tool", name: request.params.name, args: request.params.arguments });
      return new Response(
        JSON.stringify({
          jsonrpc: "2.0",
          id: request.id,
          result: { structuredContent: { tool: request.params.name } },
        }),
        { status: 200, headers },
      );
    }
    throw new Error(`synthetic transport received an unexpected method: ${request.method}`);
  };
  return { calls, fetcher };
}

function lastToolCall(transport) {
  const tools = transport.calls.filter((call) => call.kind === "tool");
  return tools[tools.length - 1];
}

// 1. The seam publishes the contracts a consumer pins against.
assert.equal(GROKPTAH_HOST_CONTRACT, "grokptah.host.v1");
assert.equal(CAPABILITY_CONTRACT, "grokptah.capabilities.v1");
assert.equal(GROKPTAH_RECOVERY_POLL_TOOL, "ptah_get_events");
assert.equal(GROKPTAH_MAX_ROUNDS, 24);

// 2. Capability descriptor types survive the package boundary intact.
const parsed = parseCapabilitySet({ contract: CAPABILITY_CONTRACT, capabilities: CAPABILITIES });
assert.ok(parsed, "consumer could not parse the capability contract");
assert.equal(parsed.contract, CAPABILITY_CONTRACT);
assert.deepStrictEqual(
  parsed.capabilities.map((entry) => [entry.id, entry.tier, entry.availability, entry.human_gate]),
  [
    ["session.observe", "observe", "available", false],
    ["run.review", "review", "available", false],
    ["run.execute", "execute", "available", false],
    ["run.queue", "execute", "available", false],
    ["agent.continuity", "observe", "available", false],
    ["agent.resume", "execute", "available", false],
    ["run.promote", "promote", "gated", true],
  ],
);
// A contract this consumer does not understand is refused, not guessed at.
assert.equal(parseCapabilitySet({ contract: "grokptah.capabilities.v2", capabilities: [] }), null);
assert.equal(parseCapabilitySet({ contract: CAPABILITY_CONTRACT, capabilities: [{ id: "bad" }] }), null);

// 3. Typed capability negotiation.
const transport = createTransport();
const host = new GrokPtahHost({
  baseUrl: "https://grokptah.invalid",
  token: "synthetic-consumer-token",
  fetcher: transport.fetcher,
  requiredCapabilities: ["run.review", "run.queue"],
});
const report = await host.connect();
assert.deepStrictEqual(report.ready, ["run.review", "run.queue"]);
assert.equal(report.contract, CAPABILITY_CONTRACT);
assert.ok(host.isConnected);

const survey = host.negotiate(["run.review", "run.promote", "computer.control"]);
assert.deepStrictEqual(survey.ready, ["run.review"]);
assert.deepStrictEqual(survey.requiresGate, ["run.promote"]);
assert.deepStrictEqual(survey.unavailable, ["computer.control"]);
assert.equal(survey.outcomes[0].descriptor.tier, "review");

// 4. An unsupported capability fails closed and tears the bearer session down.
const refusingTransport = createTransport();
const refusingHost = new GrokPtahHost({
  baseUrl: "https://grokptah.invalid",
  token: "synthetic-consumer-token",
  fetcher: refusingTransport.fetcher,
  requiredCapabilities: ["computer.control"],
});
await assert.rejects(
  refusingHost.connect(),
  (error) => error instanceof GrokPtahCapabilityError && error.state === "unavailable",
);
assert.ok(
  refusingTransport.calls.some((call) => call.kind === "close"),
  "a refused host kept its authenticated session open",
);

// 5. Malformed scope fails closed before anything reaches transport.
assert.deepStrictEqual(parseGrokPtahScope(SCOPE), SCOPE);
assert.deepStrictEqual(parseGrokPtahRunScope(RUN_SCOPE), RUN_SCOPE);
for (const malformed of [
  null,
  "consumer-session",
  { sessionId: "consumer-session" },
  { sessionId: "consumer-session", workspace: "" },
  { sessionId: "consumer-session", workspace: 1 },
  { sessionId: "consumer-session", workspace: "w", runId: "r" },
  { sessionId: "consumer-session", workspace: "w", token: "secret" },
  { sessionId: "consumer-session", workspace: "w rong" },
  { sessionId: "consumer-session", workspace: "w".repeat(513) },
]) {
  assert.equal(parseGrokPtahScope(malformed), null);
  assert.throws(() => assertGrokPtahScope(malformed), GrokPtahScopeError);
  assert.throws(() => host.workspace(malformed), GrokPtahScopeError);
}
assert.throws(() => host.run(SCOPE), GrokPtahScopeError);
assert.throws(() => validateGrokPtahBounds({ maxRounds: GROKPTAH_MAX_ROUNDS + 1 }), /at most 24/);

// 6. Scope-fenced queue and durable-agent helpers.
const workspace = host.workspace(SCOPE);
assert.deepStrictEqual(workspace.scope, SCOPE);

await workspace.queuePrompt("request-queue", "review the exact candidate", true);
assert.deepStrictEqual(lastToolCall(transport), {
  kind: "tool",
  name: "ptah_queue_prompt",
  args: {
    request_id: "request-queue",
    session_id: SCOPE.sessionId,
    workspace: SCOPE.workspace,
    prompt: "review the exact candidate",
    priority: true,
  },
});

await workspace.reorderQueue("request-reorder", "entry-1", 2, 5, 9);
assert.deepStrictEqual(lastToolCall(transport).args, {
  request_id: "request-reorder",
  session_id: SCOPE.sessionId,
  workspace: SCOPE.workspace,
  entry_id: "entry-1",
  to_index: 2,
  expected_version: 5,
  expected_revision: 9,
});

await workspace.resumePersistentAgent("agent-1", "request-resume", "continue", 8);
assert.deepStrictEqual(lastToolCall(transport), {
  kind: "tool",
  name: "ptah_resume_persistent_agent",
  args: {
    request_id: "request-resume",
    session_id: SCOPE.sessionId,
    workspace: SCOPE.workspace,
    agent_id: "agent-1",
    prompt: "continue",
    max_rounds: 8,
  },
});

// The allowlist-wide listing keeps its EmptyArgs wire schema.
await workspace.listPersistentAgents();
assert.deepStrictEqual(lastToolCall(transport), {
  kind: "tool",
  name: "ptah_list_persistent_agents",
  args: {},
});

// A bound fence still refuses a malformed run id.
assert.throws(() => workspace.run(""), GrokPtahScopeError);
assert.deepStrictEqual(workspace.run("consumer-run").scope, RUN_SCOPE);

// 7. Review and the approval gate.
const run = host.run(RUN_SCOPE);
await run.review();
assert.deepStrictEqual(lastToolCall(transport), {
  kind: "tool",
  name: "ptah_review_run",
  args: {
    session_id: RUN_SCOPE.sessionId,
    workspace: RUN_SCOPE.workspace,
    run_id: RUN_SCOPE.runId,
  },
});

await assert.rejects(
  run.approve("request-approve", "source-1", "final-1", []),
  (error) => error instanceof GrokPtahCapabilityError && error.state === "requires_gate",
);
assert.equal(lastToolCall(transport).name, "ptah_review_run", "a refused approval still called out");

await run.approve(
  "request-approve",
  "source-1",
  "final-1",
  [{ path: "src/lib/host.ts", summary: "add the seam" }],
  true,
);
assert.deepStrictEqual(lastToolCall(transport), {
  kind: "tool",
  name: "ptah_approve_run",
  args: {
    request_id: "request-approve",
    session_id: RUN_SCOPE.sessionId,
    workspace: RUN_SCOPE.workspace,
    run_id: RUN_SCOPE.runId,
    source_fingerprint: "source-1",
    final_fingerprint: "final-1",
    changed_files: [{ path: "src/lib/host.ts", summary: "add the seam" }],
  },
});

await run.promote("request-promote", "approval-1", true);
assert.equal(lastToolCall(transport).name, "ptah_promote_run");

// 8. Bounded event replay.
await run.events({ afterSeq: 3, limit: 25 });
assert.deepStrictEqual(lastToolCall(transport).args, {
  session_id: RUN_SCOPE.sessionId,
  workspace: RUN_SCOPE.workspace,
  run_id: RUN_SCOPE.runId,
  after_seq: 3,
  limit: 25,
});

// A stream must not be able to steer a trusted host into an arbitrary tool.
assert.throws(
  () =>
    run.replayRecovery({
      kind: "recovery",
      sseId: null,
      ...RUN_SCOPE,
      afterSeq: 2,
      reason: "cursor_gap",
      pollTool: "ptah_promote_run",
    }),
  /unsupported poll tool/,
);

// 9. Live monitoring folds a contiguous window and stops at recovery.
const streamTransport = createTransport({
  frames: [eventFrame(1, "started"), eventFrame(2, "progress"), recoveryFrame(2, GROKPTAH_RECOVERY_POLL_TOOL)],
});
const streamHost = new GrokPtahHost({
  baseUrl: "https://grokptah.invalid",
  token: "synthetic-consumer-token",
  fetcher: streamTransport.fetcher,
});
await streamHost.connect(["run.review"]);
const updates = [];
for await (const update of streamHost.run(RUN_SCOPE).follow()) updates.push(update);
assert.deepStrictEqual(
  updates.map((update) => [update.notification.kind, update.state.lastSeq, update.state.recoveryRequired]),
  [
    ["event", 1, false],
    ["event", 2, false],
    ["recovery", 2, true],
  ],
);
assert.equal(updates.at(-1).state.recovery.pollTool, GROKPTAH_RECOVERY_POLL_TOOL);
assert.deepStrictEqual(streamTransport.calls.find((call) => call.kind === "stream").search, {
  session_id: RUN_SCOPE.sessionId,
  workspace: RUN_SCOPE.workspace,
  run_id: RUN_SCOPE.runId,
});

// A gap marks recovery instead of guessing; a stale frame is refused outright.
const monitor = createGrokPtahRunMonitor();
const gapped = applyGrokPtahRunNotification(monitor, {
  kind: "event",
  sseId: 4,
  ...RUN_SCOPE,
  seq: 4,
  ts: "2026-08-24T00:00:00Z",
  update: {},
});
assert.equal(gapped.recoveryRequired, true);
assert.equal(
  applyGrokPtahRunNotification({ ...monitor, lastSeq: 5 }, {
    kind: "event",
    sseId: 3,
    ...RUN_SCOPE,
    seq: 3,
    ts: "2026-08-24T00:00:00Z",
    update: {},
  }),
  null,
);

// An un-negotiated host cannot stream at all.
const coldHost = new GrokPtahHost({
  baseUrl: "https://grokptah.invalid",
  token: "synthetic-consumer-token",
  fetcher: () => {
    throw new Error("cold host must not reach transport");
  },
});
assert.throws(() => coldHost.run(RUN_SCOPE).stream(), GrokPtahCapabilityError);
assert.deepStrictEqual(negotiateGrokPtahCapabilities(null, ["run.review"]).unavailable, ["run.review"]);

// 10. The browser-safe root stays free of any bearer-capable implementation.
const publicApi = await import("@grokptah/client");
const uiCoreApi = await import("@grokptah/client/ui-core");
for (const forbidden of [
  "GrokPtahClient",
  "GrokPtahOperations",
  "GrokPtahHost",
  "GrokPtahHostRun",
  "GrokPtahHostWorkspace",
  "GrokPtahScopeError",
  "GROKPTAH_HOST_CONTRACT",
  "assertGrokPtahScope",
  "requireGrokPtahCapabilities",
]) {
  assert.ok(!(forbidden in publicApi), `root package leaked ${forbidden}`);
  assert.ok(!(forbidden in uiCoreApi), `ui-core package leaked ${forbidden}`);
}
assert.equal(typeof publicApi.GrokPtahBrokerClient, "function");
assert.equal(publicApi.CAPABILITY_CONTRACT, CAPABILITY_CONTRACT);

// Read the installed artifacts through ESM resolution, the way a consumer
// bundler would, and prove the shipped bytes carry no bearer implementation.
const publicBundle = await readFile(new URL(import.meta.resolve("@grokptah/client")), "utf8");
const uiCoreBundle = await readFile(
  new URL(import.meta.resolve("@grokptah/client/ui-core")),
  "utf8",
);
for (const [label, source] of [
  ["root", publicBundle],
  ["ui-core", uiCoreBundle],
]) {
  for (const marker of [
    "Authorization",
    "Bearer",
    "mcp-session-id",
    "MCP-Protocol-Version",
    "streamRunEvents",
    "ptah_approve_run",
    "@tauri-apps",
    "GROKPTAH_HOME",
    "apiKey",
    "XAI_API_KEY",
  ]) {
    assert.ok(!source.includes(marker), `installed ${label} bundle leaked ${marker}`);
  }
}

await host.close();
console.log("trusted-host consumer fixture passed");
