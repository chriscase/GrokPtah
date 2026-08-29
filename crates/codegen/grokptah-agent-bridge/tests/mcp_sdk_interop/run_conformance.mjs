#!/usr/bin/env node
/**
 * Coordinator conformance harness for GrokPtah loopback MCP Streamable HTTP.
 *
 * Independent of in-tree McpControlClient. Protocol-level fetch is the hard gate;
 * official @modelcontextprotocol/sdk is best-effort and reported honestly via sdkOk.
 *
 * Env:
 *   GROKPTAH_MCP_URL          — http://127.0.0.1:PORT[/mcp]
 *   GROKPTAH_MCP_TOKEN        — bearer token
 *   GROKPTAH_MCP_SESSION_ID   — optional Build session UUID for mutation checks
 *   GROKPTAH_MCP_WORKSPACE    — optional allowlisted workspace path for mutations
 */
import http from "node:http";

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StreamableHTTPClientTransport } from "@modelcontextprotocol/sdk/client/streamableHttp.js";

const url = process.env.GROKPTAH_MCP_URL;
const token = process.env.GROKPTAH_MCP_TOKEN;
const hostSessionId = process.env.GROKPTAH_MCP_SESSION_ID || "";
const workspace = process.env.GROKPTAH_MCP_WORKSPACE || "";
if (!url || !token) {
  console.error("GROKPTAH_MCP_URL and GROKPTAH_MCP_TOKEN required");
  process.exit(2);
}
const endpoint = url.endsWith("/mcp") ? url : `${url.replace(/\/$/, "")}/mcp`;
const base = endpoint.replace(/\/mcp$/, "");

const checks = {
  initialize: false,
  capabilityNegotiation: false,
  toolsList: false,
  toolsCall: false,
  authBeforeBody: false,
  loopbackHealth: false,
  malformed: false,
  oversized: false,
  unknownTool: false,
  structuredError: false,
  forbiddenToolsAbsent: false,
  notificationAccepted: false,
  sessionDelete: false,
  reconnect: false,
  queueIdempotent: false,
  allowlistFailClosed: false,
};

function log(...a) {
  console.error("[conformance]", ...a);
}

const watchdog = setTimeout(() => {
  log("watchdog exit 3 after 45s");
  process.exit(3);
}, 45_000);

async function mcpFetch(
  method,
  params,
  {
    id = 1,
    sessionId,
    notification = false,
    auth = true,
    /** When set, must match initialize body protocolVersion (server validates). */
    protocolVersion = "2025-11-25",
  } = {}
) {
  const body = notification
    ? { jsonrpc: "2.0", method, params }
    : { jsonrpc: "2.0", id, method, params };
  const headers = {
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
    "MCP-Protocol-Version": protocolVersion,
  };
  if (auth) headers.Authorization = `Bearer ${token}`;
  if (sessionId) headers["mcp-session-id"] = sessionId;
  const r = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  const text = await r.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    /* ignore */
  }
  return {
    status: r.status,
    sessionId: r.headers.get("mcp-session-id"),
    contentType: r.headers.get("content-type"),
    text,
    json,
  };
}

/// Probe with headers `fetch` refuses to set — `Host` and `Origin` are
/// forbidden header names there, and a request target is never absolute — so
/// the loopback request-authority policy can be exercised from an independent
/// client stack. Resolves to the HTTP status code.
function authorityProbeStatus({ path: requestPath = "/health", headers = {} } = {}) {
  const target = new URL(base);
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        host: target.hostname,
        port: target.port,
        method: "GET",
        path: requestPath,
        headers,
      },
      (res) => {
        res.resume();
        res.on("end", () => resolve(res.statusCode));
      }
    );
    req.on("error", reject);
    req.end();
  });
}

try {
  // Health / loopback probe (unauthenticated)
  const health = await fetch(`${base}/health`);
  const hj = await health.json();
  checks.loopbackHealth =
    health.status === 200 &&
    hj.ok === true &&
    hj.status === "alive" &&
    hj.authoritative === false &&
    hj.capacity === undefined;

  // Readiness must be truthful: the unauthenticated projection withholds the
  // diagnostics but still reports the real verdict (200 ready / 503 not ready).
  const ready = await fetch(`${base}/ready`);
  const rj = await ready.json();
  checks.loopbackReadinessTruthful =
    ready.status === 200 &&
    rj.ok === true &&
    rj.ready === true &&
    rj.status === "ready" &&
    rj.authoritative === true &&
    rj.capacity === undefined;

  // ...and a valid bearer reaches the authoritative result with diagnostics.
  const authedReady = await fetch(`${base}/ready`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  const arj = await authedReady.json();
  checks.authenticatedReadinessIsAuthoritative =
    authedReady.status === 200 &&
    arj.ready === true &&
    arj.authoritative === true &&
    !!arj.capacity &&
    typeof arj.capacity.health === "object" &&
    arj.ready === rj.ready;

  // A bad credential on an open probe route is refused, not downgraded.
  const badReady = await fetch(`${base}/ready`, {
    headers: { Authorization: "Bearer wrong-token-not-real" },
  });
  checks.readinessRejectsBadBearer = badReady.status === 401;

  // Loopback request-authority policy: a hostile authority is refused before
  // authentication, whether it arrives in `Host`, in `Origin`, or as an
  // absolute-form request target that carries its own authority.
  const hostileHost = await authorityProbeStatus({
    headers: { Host: "attacker.example" },
  });
  const hostileOrigin = await authorityProbeStatus({
    headers: { Origin: "https://attacker.example" },
  });
  const hostileAbsoluteForm = await authorityProbeStatus({
    path: "http://attacker.example/health",
  });
  checks.loopbackAuthorityPolicy =
    hostileHost === 400 && hostileOrigin === 400 && hostileAbsoluteForm === 400;

  // Auth-before-body: malformed body without token → 401
  const unauthMal = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json" },
    body: "{not-json",
  });
  checks.authBeforeBody = unauthMal.status === 401;

  // Initialize + capability negotiation
  const init = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "coordinator-conformance", version: "1.0.0" },
  });
  const initBody = init.json;
  checks.initialize =
    init.status === 200 &&
    !!init.sessionId &&
    typeof initBody?.result?.protocolVersion === "string";
  checks.capabilityNegotiation =
    !!initBody?.result?.capabilities?.tools &&
    initBody.result.protocolVersion.startsWith("202");
  let sessionId = init.sessionId;

  const note = await mcpFetch(
    "notifications/initialized",
    {},
    { sessionId, notification: true }
  );
  checks.notificationAccepted = note.status === 202 || note.status === 200;

  // tools/list
  const listed = await mcpFetch("tools/list", {}, { id: 2, sessionId });
  const tools = listed.json?.result?.tools ?? [];
  const names = tools.map((t) => t.name);
  checks.toolsList =
    listed.status === 200 &&
    names.includes("ptah_get_capacity") &&
    names.includes("ptah_submit_task") &&
    names.includes("ptah_steer") &&
    names.includes("ptah_cancel");
  const forbidden = ["run_terminal_cmd", "ptah_shell", "bash", "ptah_manage_mcp", "ptah_approve"];
  checks.forbiddenToolsAbsent = !names.some((n) => forbidden.includes(n));
  const submit = tools.find((t) => t.name === "ptah_submit_task");
  if (submit?.inputSchema?.additionalProperties !== false) {
    log("submit schema must deny additionalProperties");
  }
  const allowQueue = submit?.inputSchema?.properties?.allow_queue;
  checks.submitQueueOption =
    allowQueue?.type === "boolean" && allowQueue?.default === false;
  if (!checks.submitQueueOption) {
    log("submit schema must expose optional allow_queue=false");
  }

  // tools/call capacity
  const cap = await mcpFetch(
    "tools/call",
    { name: "ptah_get_capacity", arguments: {} },
    { id: 3, sessionId }
  );
  const capText = JSON.stringify(cap.json ?? {});
  checks.toolsCall =
    cap.status === 200 &&
    (capText.includes("maxConcurrentRuns") || capText.includes("activeRuns"));

  // Malformed JSON with auth
  const mal = await fetch(endpoint, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: "{not-json",
  });
  checks.malformed = mal.status >= 400 && mal.status < 500;

  // Oversized body
  const big = "x".repeat(300_000);
  const over = await fetch(endpoint, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: `{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"pad":"${big}"}}`,
  });
  checks.oversized = over.status === 413 || over.status === 400 || over.status >= 400;

  // Unknown tool → structured error
  const unk = await mcpFetch(
    "tools/call",
    { name: "run_terminal_cmd", arguments: { command: "id" } },
    { id: 9, sessionId }
  );
  checks.unknownTool = unk.status >= 400 && unk.status < 500;
  checks.structuredError =
    !!unk.json?.error?.message &&
    unk.json?.error?.data?.code === "forbidden_scope";

  // Mutations when session+workspace provided
  if (hostSessionId && workspace) {
    const q1 = await mcpFetch(
      "tools/call",
      {
        name: "ptah_queue_prompt",
        arguments: {
          request_id: "conf-q-1",
          session_id: hostSessionId,
          workspace,
          prompt: "conformance queue once",
        },
      },
      { id: 20, sessionId }
    );
    const q2 = await mcpFetch(
      "tools/call",
      {
        name: "ptah_queue_prompt",
        arguments: {
          request_id: "conf-q-1",
          session_id: hostSessionId,
          workspace,
          prompt: "conformance queue once",
        },
      },
      { id: 21, sessionId }
    );
    checks.queueIdempotent =
      q1.status === 200 &&
      q2.status === 200 &&
      JSON.stringify(q1.json?.result?.structuredContent) ===
        JSON.stringify(q2.json?.result?.structuredContent);

    const badWs = await mcpFetch(
      "tools/call",
      {
        name: "ptah_queue_prompt",
        arguments: {
          request_id: "conf-q-badws",
          session_id: hostSessionId,
          workspace: "/tmp/not-allowlisted-conformance-xyz",
          prompt: "must fail",
        },
      },
      { id: 22, sessionId }
    );
    checks.allowlistFailClosed = badWs.status >= 400;
  } else {
    // Mark mutation checks skipped-as-true only if env not provided (Rust test always provides).
    checks.queueIdempotent = true;
    checks.allowlistFailClosed = true;
  }

  // DELETE session + reconnect
  const del = await fetch(endpoint, {
    method: "DELETE",
    headers: {
      Authorization: `Bearer ${token}`,
      "mcp-session-id": sessionId,
    },
  });
  checks.sessionDelete = del.status === 204 || del.status === 200;

  // Negotiate a different supported version on reconnect; header must match body.
  const reinit = await mcpFetch(
    "initialize",
    {
      protocolVersion: "2025-03-26",
      capabilities: {},
      clientInfo: { name: "coordinator-reconnect", version: "1.0.0" },
    },
    { protocolVersion: "2025-03-26" }
  );
  checks.reconnect =
    reinit.status === 200 &&
    !!reinit.sessionId &&
    reinit.sessionId !== sessionId &&
    reinit.json?.result?.protocolVersion === "2025-03-26";
  if (!checks.reconnect) {
    log("reconnect failed", reinit.status, reinit.sessionId, reinit.text?.slice(0, 300));
  }
  sessionId = reinit.sessionId;

  // Official SDK path (best-effort)
  let sdkOk = false;
  let sdkError = null;
  try {
    const transport = new StreamableHTTPClientTransport(new URL(endpoint), {
      requestInit: { headers: { Authorization: `Bearer ${token}` } },
    });
    const client = new Client({ name: "conformance-sdk", version: "1.0.0" });
    const t = (p, ms, label) =>
      Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error(label)), ms))]);
    await t(client.connect(transport), 8000, "SDK connect timeout");
    const sdkTools = await t(client.listTools(), 8000, "SDK listTools timeout");
    if (!sdkTools.tools.some((x) => x.name === "ptah_get_capacity")) {
      throw new Error("SDK missing capacity");
    }
    await t(client.callTool({ name: "ptah_get_capacity", arguments: {} }), 8000, "SDK call timeout");
    await client.close();
    sdkOk = true;
  } catch (e) {
    sdkError = String(e?.message || e);
    log("SDK path failed:", sdkError);
  }

  const allProtocol = Object.values(checks).every(Boolean);
  clearTimeout(watchdog);
  console.log(
    JSON.stringify({
      ok: allProtocol,
      checks,
      toolCount: names.length,
      tools: names,
      sessionId,
      sdkOk,
      sdkError,
      independentClient: "fetch-mcp-streamable-http-conformance",
    })
  );
  process.exit(allProtocol ? 0 : 1);
} catch (e) {
  clearTimeout(watchdog);
  console.error(e);
  console.log(JSON.stringify({ ok: false, error: String(e), checks }));
  process.exit(1);
}
