#!/usr/bin/env node
/**
 * Live coordinator smoke against a desktop-bootstrap MCP control server.
 *
 * Requires a server started via the same env contract as Tauri
 * (`start_control_from_env` / desktop `start_embedded_control`):
 *
 *   GROKPTAH_MCP_URL       — http://127.0.0.1:PORT[/mcp]
 *   GROKPTAH_MCP_TOKEN     — bearer (matches GROKPTAH_CONTROL_TOKEN)
 *   GROKPTAH_MCP_SESSION_ID — Build session UUID on that host
 *   GROKPTAH_MCP_WORKSPACE  — allowlisted workspace path
 *
 * Independent of McpControlClient. Protocol-level fetch is the hard gate.
 */
import fs from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";

const url = process.env.GROKPTAH_MCP_URL;
const token = process.env.GROKPTAH_MCP_TOKEN;
const hostSessionId = process.env.GROKPTAH_MCP_SESSION_ID;
const workspace = process.env.GROKPTAH_MCP_WORKSPACE;
const scopedReadTools = new Set([
  "ptah_get_run",
  "ptah_get_progress",
  "ptah_get_events",
  "ptah_get_changes",
  "ptah_get_test_results",
  "ptah_get_handoff",
  "ptah_review_run",
]);
if (!url || !token || !hostSessionId || !workspace) {
  console.error(
    "GROKPTAH_MCP_URL, GROKPTAH_MCP_TOKEN, GROKPTAH_MCP_SESSION_ID, GROKPTAH_MCP_WORKSPACE required"
  );
  process.exit(2);
}
const endpoint = url.endsWith("/mcp") ? url : `${url.replace(/\/$/, "")}/mcp`;
const base = endpoint.replace(/\/mcp$/, "");
const endpointUrl = new URL(endpoint);

const checks = {};
const steps = [];
function log(...a) {
  console.error("[live-smoke]", ...a);
}
function record(name, ok, detail) {
  checks[name] = !!ok;
  steps.push({ name, ok: !!ok, detail: detail ?? null });
  if (!ok) log("FAIL", name, detail);
  else log("ok", name, detail ?? "");
}

const watchdog = setTimeout(() => {
  log("watchdog exit 3 after 90s");
  console.log(JSON.stringify({ ok: false, reason: "watchdog", checks, steps }));
  process.exit(3);
}, 90_000);

async function mcpFetch(
  method,
  params,
  { id = 1, sessionId, notification = false, auth = true, protocolVersion = "2025-11-25" } = {}
) {
  const scopedParams =
    method === "tools/call" &&
    scopedReadTools.has(params?.name) &&
    params?.arguments?.run_id &&
    !params.arguments.session_id
      ? {
          ...params,
          arguments: {
            ...params.arguments,
            session_id: hostSessionId,
            workspace,
          },
        }
      : params;
  const body = notification
    ? { jsonrpc: "2.0", method, params: scopedParams }
    : { jsonrpc: "2.0", id, method, params: scopedParams };
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
    text,
    json,
  };
}

function structured(callJson) {
  return callJson?.result?.structuredContent ?? callJson?.result ?? null;
}

async function pollRun(mcpSession, runId, wantTerminal = true, ms = 15000) {
  const start = Date.now();
  let last = null;
  while (Date.now() - start < ms) {
    const r = await mcpFetch(
      "tools/call",
      { name: "ptah_get_run", arguments: { run_id: runId } },
      { id: Date.now() % 1e9, sessionId: mcpSession }
    );
    const sc = structured(r.json);
    last = sc;
    const state = sc?.state;
    if (
      wantTerminal &&
      ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(state)
    ) {
      return sc;
    }
    if (!wantTerminal && (state === "running" || state === "queued")) {
      return sc;
    }
    await new Promise((res) => setTimeout(res, 80));
  }
  return last;
}

try {
  // --- Health / loopback ---
  const health = await fetch(`${base}/health`);
  const hj = await health.json();
  record(
    "loopbackHealth",
    health.status === 200 && hj.ok === true && typeof hj.maxConcurrent === "number",
    { maxConcurrent: hj.maxConcurrent, sessions: hj.sessions }
  );
  // Addr must be loopback (client connected to 127.0.0.1).
  const hostPart = new URL(base).hostname;
  record(
    "loopbackOnlyBind",
    hostPart === "127.0.0.1" || hostPart === "localhost",
    hostPart
  );

  // --- Auth failures ---
  const missing = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method: "tools/list", params: {} }),
  });
  record("missingToken", missing.status === 401, missing.status);

  const wrong = await fetch(endpoint, {
    method: "POST",
    headers: {
      Authorization: "Bearer wrong-token-not-real",
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

  // --- Initialize / session ---
  const init = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "live-desktop-smoke", version: "1.0.0" },
  });
  record(
    "initialize",
    init.status === 200 && !!init.sessionId && !!init.json?.result?.protocolVersion,
    init.sessionId
  );
  let mcpSession = init.sessionId;
  const note = await mcpFetch("notifications/initialized", {}, { sessionId: mcpSession, notification: true });
  record("notification", note.status === 202 || note.status === 200, note.status);

  // --- List tools / capacity / sessions ---
  const listed = await mcpFetch("tools/list", {}, { id: 2, sessionId: mcpSession });
  const tools = listed.json?.result?.tools ?? [];
  const names = tools.map((t) => t.name);
  record(
    "toolsList",
    listed.status === 200 &&
      names.includes("ptah_list_sessions") &&
      names.includes("ptah_submit_task") &&
      !names.includes("run_terminal_cmd"),
    names.length
  );

  const cap = await mcpFetch(
    "tools/call",
    { name: "ptah_get_capacity", arguments: {} },
    { id: 3, sessionId: mcpSession }
  );
  const capSc = structured(cap.json);
  record(
    "capacity",
    cap.status === 200 &&
      (capSc?.maxConcurrentRuns != null || JSON.stringify(cap.json).includes("maxConcurrentRuns")),
    capSc
  );

  const sessions = await mcpFetch(
    "tools/call",
    { name: "ptah_list_sessions", arguments: {} },
    { id: 4, sessionId: mcpSession }
  );
  const sessSc = structured(sessions.json);
  const sessArr = Array.isArray(sessSc?.sessions) ? sessSc.sessions : [];
  const hasHostSession = sessArr.some(
    (s) => String(s.sessionId ?? "") === String(hostSessionId)
  );
  record(
    "listSessions",
    sessions.status === 200 && hasHostSession,
    {
      count: sessArr.length,
      hostSessionId,
      found: hasHostSession,
      ids: sessArr.map((s) => s.sessionId),
    }
  );

  // --- Unknown tool ---
  const unk = await mcpFetch(
    "tools/call",
    { name: "run_terminal_cmd", arguments: { command: "id" } },
    { id: 5, sessionId: mcpSession }
  );
  record(
    "unknownTool",
    unk.status >= 400 &&
      unk.status < 500 &&
      unk.json?.error?.data?.code === "forbidden_scope",
    unk.json?.error
  );

  // --- Allowlist fail-closed (plain path + symlink escape) ---
  const badWs = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: "live-bad-ws",
        session_id: hostSessionId,
        workspace: "/tmp/not-allowlisted-live-smoke-xyz",
        prompt: "must fail",
      },
    },
    { id: 6, sessionId: mcpSession }
  );
  record("allowlistFailClosed", badWs.status >= 400, badWs.status);

  // Symlink whose target resolves *outside* the allowlisted workspace root.
  let symlinkEscapeOk = false;
  let symlinkDetail = null;
  try {
    const outside = fs.mkdtempSync(path.join(os.tmpdir(), "ptah-escape-"));
    const linkPath = path.join(workspace, `escape-link-${Date.now()}`);
    fs.symlinkSync(outside, linkPath);
    const badLink = await mcpFetch(
      "tools/call",
      {
        name: "ptah_queue_prompt",
        arguments: {
          request_id: "live-symlink-escape",
          session_id: hostSessionId,
          workspace: linkPath,
          prompt: "must fail closed on symlink escape",
        },
      },
      { id: 7, sessionId: mcpSession }
    );
    const badSubmit = await mcpFetch(
      "tools/call",
      {
        name: "ptah_submit_task",
        arguments: {
          request_id: "live-symlink-escape-submit",
          session_id: hostSessionId,
          workspace: linkPath,
          prompt: "must fail closed on symlink escape",
        },
      },
      { id: 8, sessionId: mcpSession }
    );
    symlinkEscapeOk =
      badLink.status >= 400 &&
      badLink.status < 500 &&
      badSubmit.status >= 400 &&
      badSubmit.status < 500;
    symlinkDetail = {
      linkPath,
      outside,
      queueStatus: badLink.status,
      submitStatus: badSubmit.status,
      queueCode: badLink.json?.error?.data?.code,
    };
    try {
      fs.unlinkSync(linkPath);
    } catch {
      /* best-effort */
    }
    try {
      fs.rmdirSync(outside);
    } catch {
      /* best-effort */
    }
  } catch (e) {
    symlinkDetail = String(e?.message || e);
  }
  record("symlinkEscapeFailClosed", symlinkEscapeOk, symlinkDetail);

  // --- Queue + idempotent replay ---
  const qArgs = {
    request_id: "live-queue-1",
    session_id: hostSessionId,
    workspace,
    prompt: "live smoke queue follow-up",
  };
  const q1 = await mcpFetch(
    "tools/call",
    { name: "ptah_queue_prompt", arguments: qArgs },
    { id: 10, sessionId: mcpSession }
  );
  const q2 = await mcpFetch(
    "tools/call",
    { name: "ptah_queue_prompt", arguments: qArgs },
    { id: 11, sessionId: mcpSession }
  );
  record(
    "queueIdempotent",
    q1.status === 200 &&
      q2.status === 200 &&
      JSON.stringify(structured(q1.json)) === JSON.stringify(structured(q2.json)),
    { q1: q1.status, q2: q2.status }
  );

  // Conflict on same request_id
  const qConflict = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: { ...qArgs, prompt: "different payload must conflict" },
    },
    { id: 12, sessionId: mcpSession }
  );
  record("queueConflict", qConflict.status >= 400, qConflict.status);

  // --- Bounded submit + durable reads ---
  const submit = await mcpFetch(
    "tools/call",
    {
      name: "ptah_submit_task",
      arguments: {
        request_id: "live-submit-1",
        session_id: hostSessionId,
        workspace,
        prompt: "list files in the project root",
        bounds: { maxPromptBytes: 10000, maxRounds: 4, maxDurationMs: 30000 },
      },
    },
    { id: 20, sessionId: mcpSession }
  );
  const submitSc = structured(submit.json);
  const runId = submitSc?.runId ?? submitSc?.run_id;
  record("submit", submit.status === 200 && !!runId, runId);

  let terminal = null;
  if (runId) {
    terminal = await pollRun(mcpSession, runId, true, 20000);
    record(
      "durableRunTerminal",
      !!terminal &&
        ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(
          terminal.state
        ),
      terminal?.state
    );

    const progress = await mcpFetch(
      "tools/call",
      { name: "ptah_get_progress", arguments: { run_id: runId } },
      { id: 21, sessionId: mcpSession }
    );
    record("progress", progress.status === 200 && !!structured(progress.json), progress.status);

    const events = await mcpFetch(
      "tools/call",
      { name: "ptah_get_events", arguments: { run_id: runId, after_seq: 0, limit: 100 } },
      { id: 22, sessionId: mcpSession }
    );
    const evSc = structured(events.json);
    const entries = evSc?.entries ?? [];
    let ordered = true;
    let prev = 0;
    for (const e of entries) {
      if (typeof e.seq === "number") {
        if (e.seq < prev) ordered = false;
        prev = e.seq;
      }
    }
    record("eventsOrdered", events.status === 200 && ordered, {
      count: entries.length,
      ordered,
    });

    // Replay same after_seq page is stable (cursor not expired for fresh run).
    const events2 = await mcpFetch(
      "tools/call",
      { name: "ptah_get_events", arguments: { run_id: runId, after_seq: 0, limit: 100 } },
      { id: 23, sessionId: mcpSession }
    );
    record(
      "eventsReplay",
      events2.status === 200 &&
        (structured(events2.json)?.cursorExpired === false ||
          structured(events2.json)?.cursorExpired == null),
      structured(events2.json)?.cursorExpired
    );

    const changes = await mcpFetch(
      "tools/call",
      { name: "ptah_get_changes", arguments: { run_id: runId } },
      { id: 24, sessionId: mcpSession }
    );
    const chSc = structured(changes.json);
    const changeEntriesOk =
      Array.isArray(chSc?.changes) &&
      chSc.changes.every(
        (c) =>
          c &&
          typeof c === "object" &&
          typeof c.path === "string" &&
          typeof c.summary === "string"
      );
    record(
      "changes",
      changes.status === 200 &&
        chSc != null &&
        chSc.runId === runId &&
        Array.isArray(chSc.changes) &&
        changeEntriesOk,
      {
        status: changes.status,
        runId: chSc?.runId,
        changeCount: Array.isArray(chSc?.changes) ? chSc.changes.length : null,
      }
    );

    const tests = await mcpFetch(
      "tools/call",
      { name: "ptah_get_test_results", arguments: { run_id: runId } },
      { id: 25, sessionId: mcpSession }
    );
    const tSc = structured(tests.json);
    record(
      "testResults",
      tests.status === 200 &&
        tSc != null &&
        tSc.runId === runId &&
        typeof tSc.status === "string" &&
        Array.isArray(tSc.results),
      {
        status: tests.status,
        runId: tSc?.runId,
        obsStatus: tSc?.status,
        resultCount: Array.isArray(tSc?.results) ? tSc.results.length : null,
      }
    );

    const handoff = await mcpFetch(
      "tools/call",
      { name: "ptah_get_handoff", arguments: { run_id: runId } },
      { id: 26, sessionId: mcpSession }
    );
    const hSc = structured(handoff.json);
    record(
      "handoff",
      handoff.status === 200 &&
        hSc != null &&
        hSc.runId === runId &&
        hSc.state != null,
      { status: handoff.status, state: hSc?.state }
    );

    // Idempotent submit replay
    const submitReplay = await mcpFetch(
      "tools/call",
      {
        name: "ptah_submit_task",
        arguments: {
          request_id: "live-submit-1",
          session_id: hostSessionId,
          workspace,
          prompt: "list files in the project root",
          bounds: { maxPromptBytes: 10000, maxRounds: 4, maxDurationMs: 30000 },
        },
      },
      { id: 27, sessionId: mcpSession }
    );
    const replayId =
      structured(submitReplay.json)?.runId ?? structured(submitReplay.json)?.run_id;
    record("submitIdempotent", submitReplay.status === 200 && replayId === runId, replayId);
  } else {
    record("durableRunTerminal", false, "no runId");
    record("progress", false);
    record("eventsOrdered", false);
    record("eventsReplay", false);
    record("changes", false);
    record("testResults", false);
    record("handoff", false);
    record("submitIdempotent", false);
  }

  // --- Busy path: long shell, steer, cancel ---
  const busySubmit = await mcpFetch(
    "tools/call",
    {
      name: "ptah_submit_task",
      arguments: {
        request_id: "live-busy-1",
        session_id: hostSessionId,
        workspace,
        prompt: "run (sleep 4; echo live-smoke-busy) & wait",
        bounds: { maxPromptBytes: 50000, maxRounds: 8, maxDurationMs: 60000 },
      },
    },
    { id: 30, sessionId: mcpSession }
  );
  const busyId =
    structured(busySubmit.json)?.runId ?? structured(busySubmit.json)?.run_id;
  record("busySubmit", busySubmit.status === 200 && !!busyId, busyId);

  if (busyId) {
    await new Promise((r) => setTimeout(r, 100));
    // Non-cancelling steer during busy turn
    const steer = await mcpFetch(
      "tools/call",
      {
        name: "ptah_steer",
        arguments: {
          request_id: "live-steer-1",
          session_id: hostSessionId,
          workspace,
          text: "prefer finishing quickly",
        },
      },
      { id: 31, sessionId: mcpSession }
    );
    const steerSc = structured(steer.json);
    const disposition = steerSc?.disposition;
    record(
      "steerNonCancelling",
      steer.status === 200 &&
        (disposition === "pending" || disposition === "queued"),
      disposition
    );
    // Run must not be cancelled solely by steer
    const mid = await mcpFetch(
      "tools/call",
      { name: "ptah_get_run", arguments: { run_id: busyId } },
      { id: 32, sessionId: mcpSession }
    );
    const midState = structured(mid.json)?.state;
    record(
      "steerDidNotCancel",
      midState !== "cancelled",
      midState
    );

    const cancel = await mcpFetch(
      "tools/call",
      {
        name: "ptah_cancel",
        arguments: {
          request_id: "live-cancel-1",
          session_id: hostSessionId,
          workspace,
          run_id: busyId,
        },
      },
      { id: 33, sessionId: mcpSession }
    );
    record("cancel", cancel.status === 200 && structured(cancel.json)?.cancelled === true, {
      status: cancel.status,
      sc: structured(cancel.json),
    });

    const cancelReplay = await mcpFetch(
      "tools/call",
      {
        name: "ptah_cancel",
        arguments: {
          request_id: "live-cancel-1",
          session_id: hostSessionId,
          workspace,
          run_id: busyId,
        },
      },
      { id: 34, sessionId: mcpSession }
    );
    record(
      "cancelIdempotent",
      cancelReplay.status === 200 &&
        JSON.stringify(structured(cancel.json)) ===
          JSON.stringify(structured(cancelReplay.json)),
      cancelReplay.status
    );

    const cancelledRun = await pollRun(mcpSession, busyId, true, 10000);
    record("cancelDurable", cancelledRun?.state === "cancelled", cancelledRun?.state);
  } else {
    record("steerNonCancelling", false, "no busy run");
    record("steerDidNotCancel", false);
    record("cancel", false);
    record("cancelIdempotent", false);
    record("cancelDurable", false);
  }

  // --- Session DELETE + reconnect / stale ---
  const del = await fetch(endpoint, {
    method: "DELETE",
    headers: {
      Authorization: `Bearer ${token}`,
      "mcp-session-id": mcpSession,
    },
  });
  record("sessionDelete", del.status === 204 || del.status === 200, del.status);

  const stale = await mcpFetch(
    "tools/list",
    {},
    { id: 40, sessionId: mcpSession }
  );
  record("staleSessionFailClosed", stale.status >= 400, stale.status);

  const reinit = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "live-smoke-reconnect", version: "1.0.0" },
  });
  record(
    "reconnect",
    reinit.status === 200 && !!reinit.sessionId && reinit.sessionId !== mcpSession,
    reinit.sessionId
  );
  mcpSession = reinit.sessionId;

  // --- Real client disconnect mid-request + idempotent retry (no double mutation) ---
  // Full POST body is written then the TCP connection is dropped without reading
  // the response; retry with the same request_id must not double-enqueue.
  const discReqId = `live-disconnect-retry-${Date.now()}`;
  const discBody = JSON.stringify({
    jsonrpc: "2.0",
    id: 50,
    method: "tools/call",
    params: {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: discReqId,
        session_id: hostSessionId,
        workspace,
        prompt: "queued once despite disconnect",
      },
    },
  });
  const discPayload = [
    `POST ${endpointUrl.pathname} HTTP/1.1`,
    `Host: ${endpointUrl.host}`,
    `Authorization: Bearer ${token}`,
    "Content-Type: application/json",
    "Accept: application/json",
    `Content-Length: ${Buffer.byteLength(discBody)}`,
    "Connection: close",
    "",
    discBody,
  ].join("\r\n");
  await new Promise((resolve, reject) => {
    const sock = net.connect(
      { host: endpointUrl.hostname, port: Number(endpointUrl.port) },
      () => {
        sock.write(discPayload, () => {
          // Drop without reading response = client disconnect.
          sock.destroy();
          resolve();
        });
      }
    );
    sock.on("error", (err) => {
      // ECONNRESET after destroy is fine.
      if (err.code === "ECONNRESET" || err.code === "EPIPE") resolve();
      else reject(err);
    });
  });
  await new Promise((r) => setTimeout(r, 150));
  const discRetry = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: discReqId,
        session_id: hostSessionId,
        workspace,
        prompt: "queued once despite disconnect",
      },
    },
    { id: 51, sessionId: mcpSession }
  );
  const discConflict = await mcpFetch(
    "tools/call",
    {
      name: "ptah_queue_prompt",
      arguments: {
        request_id: discReqId,
        session_id: hostSessionId,
        workspace,
        prompt: "different payload after disconnect",
      },
    },
    { id: 52, sessionId: mcpSession }
  );
  record(
    "disconnectMidRequestIdempotentRetry",
    discRetry.status === 200 && discConflict.status >= 400,
    {
      retryStatus: discRetry.status,
      conflictStatus: discConflict.status,
      requestId: discReqId,
    }
  );

  // --- Malformed with auth ---
  const mal = await fetch(endpoint, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${token}`,
      "Content-Type": "application/json",
    },
    body: "{not-json",
  });
  record("malformedAuthed", mal.status >= 400 && mal.status < 500, mal.status);

  const failed = Object.entries(checks).filter(([, v]) => !v).map(([k]) => k);
  const ok = failed.length === 0;
  clearTimeout(watchdog);
  console.log(
    JSON.stringify({
      ok,
      failed,
      checks,
      steps,
      tools: names,
      independentClient: "fetch-live-desktop-smoke",
    })
  );
  process.exit(ok ? 0 : 1);
} catch (e) {
  clearTimeout(watchdog);
  console.error(e);
  console.log(JSON.stringify({ ok: false, error: String(e), checks, steps }));
  process.exit(1);
}
