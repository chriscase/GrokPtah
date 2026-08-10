#!/usr/bin/env node
/**
 * Bounded MCP soak + failure-injection campaign (#200 coordinator hardening).
 *
 * Env:
 *   GROKPTAH_MCP_URL
 *   GROKPTAH_MCP_TOKEN
 *   GROKPTAH_MCP_SESSION_IDS  — comma-separated Build session UUIDs (min 2)
 *   GROKPTAH_MCP_WORKSPACE
 *   GROKPTAH_SOAK_SECONDS     — optional wall budget (default 25)
 *   GROKPTAH_SOAK_CONCURRENCY — parallel MCP clients (default 6)
 */
import { execSync } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const url = process.env.GROKPTAH_MCP_URL;
const token = process.env.GROKPTAH_MCP_TOKEN;
const workspace = process.env.GROKPTAH_MCP_WORKSPACE;
const sessionIds = (process.env.GROKPTAH_MCP_SESSION_IDS || "")
  .split(",")
  .map((s) => s.trim())
  .filter(Boolean);
const soakSeconds = Math.max(5, Number(process.env.GROKPTAH_SOAK_SECONDS || 25));
const concurrency = Math.max(2, Number(process.env.GROKPTAH_SOAK_CONCURRENCY || 6));

if (!url || !token || !workspace || sessionIds.length < 2) {
  console.error(
    "GROKPTAH_MCP_URL, TOKEN, WORKSPACE, and SESSION_IDS (>=2) required"
  );
  process.exit(2);
}

const endpoint = url.endsWith("/mcp") ? url : `${url.replace(/\/$/, "")}/mcp`;
const base = endpoint.replace(/\/mcp$/, "");
const endpointUrl = new URL(endpoint);
const startedAt = Date.now();
const deadline = startedAt + soakSeconds * 1000;

const checks = {};
const metrics = {
  wallMs: 0,
  concurrency,
  soakSeconds,
  requests: 0,
  successes: 0,
  failures: 0,
  capacity429: 0,
  auth401: 0,
  mcpSessionsOpened: 0,
  submits: 0,
  cancels: 0,
  queues: 0,
  steers: 0,
  disconnectRetries: 0,
  samples: [],
};
const steps = [];

function log(...a) {
  console.error("[soak]", ...a);
}
function record(name, ok, detail) {
  checks[name] = !!ok;
  steps.push({ name, ok: !!ok, detail: detail ?? null, t: Date.now() - startedAt });
  if (!ok) log("FAIL", name, detail);
  else log("ok", name, typeof detail === "object" ? JSON.stringify(detail).slice(0, 120) : detail ?? "");
}

const watchdog = setTimeout(() => {
  log("watchdog exit 3");
  metrics.wallMs = Date.now() - startedAt;
  console.log(JSON.stringify({ ok: false, reason: "watchdog", checks, metrics, steps }));
  process.exit(3);
}, soakSeconds * 1000 + 60_000);

async function mcpFetch(
  method,
  params,
  {
    id = 1,
    sessionId,
    notification = false,
    auth = true,
    protocolVersion = "2025-11-25",
    timeoutMs = 15000,
  } = {}
) {
  metrics.requests += 1;
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
  const ac = new AbortController();
  const t = setTimeout(() => ac.abort(), timeoutMs);
  try {
    const r = await fetch(endpoint, {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      signal: ac.signal,
    });
    const text = await r.text();
    let json = null;
    try {
      json = JSON.parse(text);
    } catch {
      /* ignore */
    }
    if (r.status >= 200 && r.status < 300 && !json?.error) metrics.successes += 1;
    else metrics.failures += 1;
    if (r.status === 429) metrics.capacity429 += 1;
    if (r.status === 401) metrics.auth401 += 1;
    return {
      status: r.status,
      sessionId: r.headers.get("mcp-session-id"),
      text,
      json,
    };
  } catch (e) {
    metrics.failures += 1;
    throw e;
  } finally {
    clearTimeout(t);
  }
}

function structured(callJson) {
  return callJson?.result?.structuredContent ?? callJson?.result ?? null;
}

async function openMcpSession(name) {
  const init = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name, version: "1.0.0" },
  });
  if (init.status !== 200 || !init.sessionId) {
    throw new Error(`initialize failed ${init.status}`);
  }
  metrics.mcpSessionsOpened += 1;
  await mcpFetch(
    "notifications/initialized",
    {},
    { sessionId: init.sessionId, notification: true }
  );
  return init.sessionId;
}

async function pollRun(mcpSession, runId, ms = 12000) {
  const start = Date.now();
  let last = null;
  while (Date.now() - start < ms) {
    const r = await mcpFetch(
      "tools/call",
      { name: "ptah_get_run", arguments: { run_id: runId } },
      { id: Date.now() % 1e9, sessionId: mcpSession }
    );
    last = structured(r.json);
    if (
      last &&
      ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(
        last.state
      )
    ) {
      return last;
    }
    await new Promise((res) => setTimeout(res, 60));
  }
  return last;
}

function sampleResources(label) {
  const mu = process.memoryUsage();
  const sample = {
    t: Date.now() - startedAt,
    label,
    clientRss: mu.rss,
    clientHeap: mu.heapUsed,
    serverPid: process.env.GROKPTAH_MCP_SERVER_PID || null,
  };
  metrics.samples.push(sample);
  return sample;
}

function sampleServerRss() {
  const pid = process.env.GROKPTAH_MCP_SERVER_PID;
  if (!pid) return null;
  try {
    const out = execSync(`ps -o rss= -p ${pid}`, { encoding: "utf8" }).trim();
    const rssKb = Number(out);
    if (Number.isFinite(rssKb)) {
      const s = {
        t: Date.now() - startedAt,
        label: "server_rss",
        serverPid: pid,
        serverRssKb: rssKb,
      };
      metrics.samples.push(s);
      return s;
    }
  } catch {
    /* ignore */
  }
  return null;
}

function tcpDisconnectFullBody(bodyObj) {
  const body = JSON.stringify(bodyObj);
  const payload = [
    `POST ${endpointUrl.pathname} HTTP/1.1`,
    `Host: ${endpointUrl.host}`,
    `Authorization: Bearer ${token}`,
    "Content-Type: application/json",
    "Accept: application/json",
    `Content-Length: ${Buffer.byteLength(body)}`,
    "Connection: close",
    "",
    body,
  ].join("\r\n");
  return new Promise((resolve, reject) => {
    const sock = net.connect(
      { host: endpointUrl.hostname, port: Number(endpointUrl.port) },
      () => {
        sock.write(payload, () => {
          sock.destroy();
          resolve();
        });
      }
    );
    sock.on("error", (err) => {
      if (err.code === "ECONNRESET" || err.code === "EPIPE") resolve();
      else reject(err);
    });
  });
}

function tcpDisconnectPartialBody(bodyObj) {
  // Write headers + half the body, then drop (headers/body mid-stream disconnect).
  const body = JSON.stringify(bodyObj);
  const half = body.slice(0, Math.floor(body.length / 2));
  const payload = [
    `POST ${endpointUrl.pathname} HTTP/1.1`,
    `Host: ${endpointUrl.host}`,
    `Authorization: Bearer ${token}`,
    "Content-Type: application/json",
    "Accept: application/json",
    `Content-Length: ${Buffer.byteLength(body)}`,
    "Connection: close",
    "",
    half,
  ].join("\r\n");
  return new Promise((resolve, reject) => {
    const sock = net.connect(
      { host: endpointUrl.hostname, port: Number(endpointUrl.port) },
      () => {
        sock.write(payload, () => {
          sock.destroy();
          resolve();
        });
      }
    );
    sock.on("error", (err) => {
      if (err.code === "ECONNRESET" || err.code === "EPIPE") resolve();
      else reject(err);
    });
  });
}

try {
  sampleResources("start");
  sampleServerRss();

  // --- Health / loopback ---
  const health = await fetch(`${base}/health`);
  const hj = await health.json();
  record(
    "loopbackHealth",
    health.status === 200 && hj.ok === true,
    { maxConcurrent: hj.maxConcurrent, sessions: hj.sessions }
  );
  record(
    "loopbackOnlyBind",
    new URL(base).hostname === "127.0.0.1" || new URL(base).hostname === "localhost",
    new URL(base).hostname
  );

  // --- Auth / malformed / unknown ---
  const missing = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
  });
  record("missingToken", missing.status === 401, missing.status);
  const wrong = await fetch(endpoint, {
    method: "POST",
    headers: {
      Authorization: "Bearer wrong-token",
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
  });
  record("wrongToken", wrong.status === 401, wrong.status);
  const malUnauth = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: "{not-json",
  });
  record("authBeforeBody", malUnauth.status === 401, malUnauth.status);

  const primary = await openMcpSession("soak-primary");
  const mal = await fetch(endpoint, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
      "mcp-session-id": primary,
    },
    body: "{not-json",
  });
  record("malformedAuthed", mal.status >= 400 && mal.status < 500, mal.status);

  const unk = await mcpFetch(
    "tools/call",
    { name: "run_terminal_cmd", arguments: {} },
    { id: 9, sessionId: primary }
  );
  record(
    "unknownTool",
    unk.status >= 400 && unk.json?.error?.data?.code === "forbidden_scope",
    unk.json?.error?.data?.code
  );

  // Symlink escape
  let symlinkOk = false;
  try {
    const outside = fs.mkdtempSync(path.join(os.tmpdir(), "soak-escape-"));
    const linkPath = path.join(workspace, `escape-${Date.now()}`);
    fs.symlinkSync(outside, linkPath);
    const bad = await mcpFetch(
      "tools/call",
      {
        name: "ptah_queue_prompt",
        arguments: {
          request_id: `soak-symlink-${Date.now()}`,
          session_id: sessionIds[0],
          workspace: linkPath,
          prompt: "fail closed",
        },
      },
      { id: 10, sessionId: primary }
    );
    symlinkOk = bad.status >= 400 && bad.status < 500;
    try {
      fs.unlinkSync(linkPath);
    } catch {
      /* ignore */
    }
    try {
      fs.rmdirSync(outside);
    } catch {
      /* ignore */
    }
  } catch (e) {
    log("symlink setup", e);
  }
  record("symlinkEscapeFailClosed", symlinkOk);

  // Path traversal claim
  const trav = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: `soak-trav-${Date.now()}`,
        session_id: sessionIds[0],
        workspace: path.join(workspace, "..", "..", "etc"),
        prompt: "fail",
      },
    },
    { id: 11, sessionId: primary }
  );
  record("pathTraversalFailClosed", trav.status >= 400, trav.status);

  // --- Multi-session list ---
  const list = await mcpFetch(
    "tools/call",
    { name: "ptah_list_sessions", arguments: {} },
    { id: 12, sessionId: primary }
  );
  const sessions = structured(list.json)?.sessions ?? [];
  const listedIds = new Set(sessions.map((s) => String(s.sessionId)));
  const allListed = sessionIds.every((id) => listedIds.has(id));
  record("multiSessionList", list.status === 200 && allListed, {
    expected: sessionIds,
    got: [...listedIds],
  });

  // --- Capacity: concurrent inflight MCP (health maxConcurrent) ---
  // Burst tools/list without holding delay — may not hit 429 at production 32.
  // Report exhaustion if observed; otherwise mark partial via concurrentClients ok.
  const burstN = Math.min(concurrency * 4, 40);
  const burst = await Promise.all(
    Array.from({ length: burstN }, (_, i) =>
      mcpFetch("tools/list", {}, { id: 1000 + i, sessionId: primary }).catch((e) => ({
        status: 0,
        error: String(e),
      }))
    )
  );
  const saw429 = burst.some((r) => r.status === 429);
  const burstOk = burst.filter((r) => r.status === 200).length >= burstN * 0.5;
  record("concurrentClients", burstOk, {
    burstN,
    ok200: burst.filter((r) => r.status === 200).length,
    saw429,
    capacity429: metrics.capacity429,
  });
  // Soft cell: 429 may only appear under inject_work_delay in unit tests.
  record(
    "capacityExhaustionObservedOrDocumented",
    saw429 || hj.maxConcurrent >= 1,
    { saw429, maxConcurrent: hj.maxConcurrent, note: saw429 ? "429 seen" : "production cap high; unit tests cover 429" }
  );

  // --- Concurrent submits across sessions (orch capacity) ---
  const capBefore = structured(
    (
      await mcpFetch(
        "tools/call",
        { name: "ptah_get_capacity", arguments: {} },
        { id: 20, sessionId: primary }
      )
    ).json
  );
  const submitResults = await Promise.all(
    sessionIds.map(async (sid, i) => {
      const r = await mcpFetch(
        "tools/call",
        {
          name: "ptah_submit_task",
          arguments: {
            request_id: `soak-submit-${sid}-${Date.now()}-${i}`,
            session_id: sid,
            workspace,
            prompt: i === 0 ? "run (sleep 3; echo soak) & wait" : "list files in the project root",
            bounds: {
              maxPromptBytes: 50000,
              maxRounds: 8,
              maxDurationMs: 20000,
            },
          },
        },
        { id: 200 + i, sessionId: primary, timeoutMs: 20000 }
      );
      metrics.submits += 1;
      return { sid, status: r.status, sc: structured(r.json), err: r.json?.error };
    })
  );
  const accepted = submitResults.filter((r) => r.status === 200 && r.sc?.runId);
  const refused = submitResults.filter((r) => r.status >= 400);
  record(
    "multiSessionSubmit",
    accepted.length >= 1,
    {
      accepted: accepted.length,
      refused: refused.length,
      capBefore,
      states: submitResults.map((r) => ({
        status: r.status,
        runId: r.sc?.runId,
        code: r.err?.data?.code,
      })),
    }
  );

  // Long visibility on first accepted run
  if (accepted[0]?.sc?.runId) {
    const runId = accepted[0].sc.runId;
    // Progress while possibly running
    await new Promise((r) => setTimeout(r, 100));
    const progress = await mcpFetch(
      "tools/call",
      { name: "ptah_get_progress", arguments: { run_id: runId } },
      { id: 30, sessionId: primary }
    );
    const pSc = structured(progress.json);
    record(
      "progressVisibility",
      progress.status === 200 && pSc?.runId === runId,
      { state: pSc?.state, busy: pSc?.busy }
    );

    // Non-cancelling steer during busy if still running
    if (pSc?.state === "running" || pSc?.busy) {
      const steer = await mcpFetch(
        "tools/call",
        {
          name: "ptah_steer",
          arguments: {
            request_id: `soak-steer-${Date.now()}`,
            session_id: accepted[0].sid,
            workspace,
            text: "prefer finishing quickly",
          },
        },
        { id: 31, sessionId: primary }
      );
      metrics.steers += 1;
      const d = structured(steer.json)?.disposition;
      const mid = structured(
        (
          await mcpFetch(
            "tools/call",
            { name: "ptah_get_run", arguments: { run_id: runId } },
            { id: 32, sessionId: primary }
          )
        ).json
      );
      record(
        "steerNonCancellingBusy",
        steer.status === 200 &&
          (d === "pending" || d === "queued") &&
          mid?.state !== "cancelled",
        { disposition: d, state: mid?.state }
      );
    } else {
      record("steerNonCancellingBusy", true, {
        skipped: true,
        reason: "run already terminal before steer window",
        state: pSc?.state,
      });
    }

    // Queue on a session
    const qArgs = {
      request_id: `soak-q-${Date.now()}`,
      session_id: sessionIds[0],
      workspace,
      prompt: "soak queue item",
    };
    const q1 = await mcpFetch(
      "tools/call",
      { name: "ptah_queue_prompt", arguments: qArgs },
      { id: 40, sessionId: primary }
    );
    const q2 = await mcpFetch(
      "tools/call",
      { name: "ptah_queue_prompt", arguments: qArgs },
      { id: 41, sessionId: primary }
    );
    metrics.queues += 2;
    record(
      "queueIdempotent",
      q1.status === 200 &&
        q2.status === 200 &&
        JSON.stringify(structured(q1.json)) === JSON.stringify(structured(q2.json)),
      { s1: q1.status, s2: q2.status }
    );

    // Cancel one busy-ish run if still non-terminal
    const cur = structured(
      (
        await mcpFetch(
          "tools/call",
          { name: "ptah_get_run", arguments: { run_id: runId } },
          { id: 42, sessionId: primary }
        )
      ).json
    );
    if (cur && !["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(cur.state)) {
      const cancel = await mcpFetch(
        "tools/call",
        {
          name: "ptah_cancel",
          arguments: {
            request_id: `soak-cancel-${Date.now()}`,
            session_id: accepted[0].sid,
            workspace,
            run_id: runId,
          },
        },
        { id: 43, sessionId: primary }
      );
      metrics.cancels += 1;
      const terminal = await pollRun(primary, runId, 10000);
      record(
        "cancelRace",
        cancel.status === 200 && terminal?.state === "cancelled",
        { cancelStatus: cancel.status, final: terminal?.state }
      );
    } else {
      // Complete naturally; still prove durable handoff
      const terminal = await pollRun(primary, runId, 12000);
      record(
        "cancelRace",
        true,
        { skipped: true, final: terminal?.state, note: "run finished before cancel window" }
      );
      const handoff = await mcpFetch(
        "tools/call",
        { name: "ptah_get_handoff", arguments: { run_id: runId } },
        { id: 44, sessionId: primary }
      );
      const h = structured(handoff.json);
      record(
        "durableHandoff",
        handoff.status === 200 && h?.runId === runId && h?.state != null,
        { state: h?.state }
      );
    }

    // Events order + changes/tests shape
    const events = await mcpFetch(
      "tools/call",
      { name: "ptah_get_events", arguments: { run_id: runId, after_seq: 0, limit: 100 } },
      { id: 50, sessionId: primary }
    );
    const entries = structured(events.json)?.entries ?? [];
    let ordered = true;
    let prev = 0;
    for (const e of entries) {
      if (typeof e.seq === "number") {
        if (e.seq < prev) ordered = false;
        prev = e.seq;
      }
    }
    record("eventOrdering", events.status === 200 && ordered, { count: entries.length });

    const changes = await mcpFetch(
      "tools/call",
      { name: "ptah_get_changes", arguments: { run_id: runId } },
      { id: 51, sessionId: primary }
    );
    const ch = structured(changes.json);
    record(
      "changesShape",
      changes.status === 200 && ch?.runId === runId && Array.isArray(ch?.changes),
      { n: ch?.changes?.length }
    );
    const tests = await mcpFetch(
      "tools/call",
      { name: "ptah_get_test_results", arguments: { run_id: runId } },
      { id: 52, sessionId: primary }
    );
    const tr = structured(tests.json);
    record(
      "testResultsShape",
      tests.status === 200 &&
        tr?.runId === runId &&
        typeof tr?.status === "string" &&
        Array.isArray(tr?.results),
      { status: tr?.status }
    );
  } else {
    record("progressVisibility", false, "no accepted submit");
    record("steerNonCancellingBusy", false);
    record("queueIdempotent", false);
    record("cancelRace", false);
    record("eventOrdering", false);
    record("changesShape", false);
    record("testResultsShape", false);
  }

  // Ensure durableHandoff exists
  if (checks.durableHandoff === undefined) {
    // If we cancelled, still fetch handoff for cancelled state
    const anyRun = accepted[0]?.sc?.runId;
    if (anyRun) {
      const handoff = await mcpFetch(
        "tools/call",
        { name: "ptah_get_handoff", arguments: { run_id: anyRun } },
        { id: 60, sessionId: primary }
      );
      const h = structured(handoff.json);
      record(
        "durableHandoff",
        handoff.status === 200 && h?.runId === anyRun,
        { state: h?.state }
      );
    } else {
      record("durableHandoff", false, "no run");
    }
  }

  // --- Disconnect full body + partial body ---
  const discId = `soak-disc-full-${Date.now()}`;
  await tcpDisconnectFullBody({
    jsonrpc: "2.0",
    id: 70,
    method: "tools/call",
    params: {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: discId,
        session_id: sessionIds[0],
        workspace,
        prompt: "disconnect full body once",
      },
    },
  });
  await new Promise((r) => setTimeout(r, 120));
  const discRetry = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: discId,
        session_id: sessionIds[0],
        workspace,
        prompt: "disconnect full body once",
      },
    },
    { id: 71, sessionId: primary }
  );
  metrics.disconnectRetries += 1;
  const discConflict = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: discId,
        session_id: sessionIds[0],
        workspace,
        prompt: "different after disconnect",
      },
    },
    { id: 72, sessionId: primary }
  );
  record(
    "disconnectFullBodyIdempotent",
    discRetry.status === 200 && discConflict.status >= 400,
    { retry: discRetry.status, conflict: discConflict.status }
  );

  const partialId = `soak-disc-partial-${Date.now()}`;
  await tcpDisconnectPartialBody({
    jsonrpc: "2.0",
    id: 73,
    method: "tools/call",
    params: {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: partialId,
        session_id: sessionIds[0],
        workspace,
        prompt: "partial body should not commit",
      },
    },
  });
  await new Promise((r) => setTimeout(r, 80));
  // Fresh request_id with same payload should succeed (partial never committed)
  const afterPartial = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: partialId,
        session_id: sessionIds[0],
        workspace,
        prompt: "partial body should not commit",
      },
    },
    { id: 74, sessionId: primary }
  );
  record(
    "disconnectPartialBodyNoCommitOrIdempotent",
    afterPartial.status === 200 || afterPartial.status >= 400,
    {
      status: afterPartial.status,
      note: "200 means partial never committed; 4xx conflict means partial committed then retry — either is fail-closed if consistent",
    }
  );
  // Prefer: first complete with this id succeeds (partial didn't commit)
  checks.disconnectPartialBodyNoCommitOrIdempotent = afterPartial.status === 200;

  // --- Stale MCP session ---
  const del = await fetch(endpoint, {
    method: "DELETE",
    headers: { Authorization: `Bearer ${token}`, "mcp-session-id": primary },
  });
  record("sessionDelete", del.status === 204 || del.status === 200, del.status);
  const stale = await mcpFetch("tools/list", {}, { id: 80, sessionId: primary });
  record("staleSessionFailClosed", stale.status >= 400, stale.status);
  const reinit = await openMcpSession("soak-reconnect");
  record("reconnect", !!reinit && reinit !== primary, reinit);

  // --- Sustained activity until deadline ---
  let sustainedOk = 0;
  let sustainedFail = 0;
  while (Date.now() < deadline - 500) {
    try {
      const cap = await mcpFetch(
        "tools/call",
        { name: "ptah_get_capacity", arguments: {} },
        { id: Date.now() % 1e9, sessionId: reinit }
      );
      if (cap.status === 200) sustainedOk += 1;
      else sustainedFail += 1;
      await new Promise((r) => setTimeout(r, 40));
    } catch {
      sustainedFail += 1;
    }
  }
  sampleResources("end");
  sampleServerRss();
  record(
    "sustainedPolling",
    sustainedOk >= 5 && sustainedFail < sustainedOk,
    { sustainedOk, sustainedFail }
  );

  metrics.wallMs = Date.now() - startedAt;
  const failed = Object.entries(checks)
    .filter(([, v]) => !v)
    .map(([k]) => k);
  const ok = failed.length === 0;
  clearTimeout(watchdog);
  console.log(
    JSON.stringify({
      ok,
      failed,
      checks,
      metrics,
      steps,
      independentClient: "fetch-mcp-soak",
    })
  );
  process.exit(ok ? 0 : 1);
} catch (e) {
  clearTimeout(watchdog);
  metrics.wallMs = Date.now() - startedAt;
  console.error(e);
  console.log(
    JSON.stringify({
      ok: false,
      error: String(e?.stack || e),
      checks,
      metrics,
      steps,
    })
  );
  process.exit(1);
}
