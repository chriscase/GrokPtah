#!/usr/bin/env node
// Independent live client for the read-only Computer Run MCP tools (#271).
//
// Talks raw Streamable HTTP JSON-RPC against a desktop-equivalent server
// started by `start_control_from_env` (driven by the Rust test
// `live_computer_reads_node_smoke`). No Rust client library or SDK sits in
// between, so this is an outside observer's view of the contract:
// discovery, scoped listing, redaction-pinned projections, cursor paging,
// duplicate-request replay, stale-cursor recovery, cross-session and
// cross-workspace rejection, capacity, auth-before-body, and reconnect.
//
// Env:
//   GROKPTAH_MCP_URL            — http://127.0.0.1:PORT/mcp
//   GROKPTAH_MCP_TOKEN          — bearer (matches GROKPTAH_CONTROL_TOKEN)
//   GROKPTAH_MCP_SESSION_ID     — owning Build session UUID
//   GROKPTAH_MCP_WORKSPACE      — that session's allowlisted workspace
//   GROKPTAH_MCP_OTHER_WORKSPACE— a different allowlisted workspace
//   GROKPTAH_MCP_COMPUTER_RUN_A — observed run owned by the session
//   GROKPTAH_MCP_COMPUTER_RUN_B — run owned by another session
//   GROKPTAH_MCP_COMPUTER_RUN_C — run whose oldest journal entries are evicted

const url = process.env.GROKPTAH_MCP_URL;
const token = process.env.GROKPTAH_MCP_TOKEN;
const sessionId = process.env.GROKPTAH_MCP_SESSION_ID;
const workspace = process.env.GROKPTAH_MCP_WORKSPACE;
const otherWorkspace = process.env.GROKPTAH_MCP_OTHER_WORKSPACE;
const runA = process.env.GROKPTAH_MCP_COMPUTER_RUN_A;
const runB = process.env.GROKPTAH_MCP_COMPUTER_RUN_B;
const runC = process.env.GROKPTAH_MCP_COMPUTER_RUN_C;

if (!url || !token || !sessionId || !workspace || !otherWorkspace || !runA || !runB || !runC) {
  console.error("missing required GROKPTAH_MCP_* environment");
  process.exit(2);
}

const failed = [];
const passed = [];
function check(name, condition, detail) {
  if (condition) {
    passed.push(name);
  } else {
    failed.push({ name, detail: detail ?? null });
  }
}

async function rpc(id, method, params, options = {}) {
  const headers = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${options.token ?? token}`,
  };
  if (options.session) headers["mcp-session-id"] = options.session;
  const response = await fetch(url, {
    method: "POST",
    headers,
    body: JSON.stringify({ jsonrpc: "2.0", id, method, params }),
  });
  const text = await response.text();
  let body = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  return { status: response.status, headers: response.headers, body };
}

function callArgs(extra) {
  return { session_id: sessionId, workspace, ...extra };
}

async function tool(id, name, args, options = {}) {
  const { status, body } = await rpc(
    id,
    "tools/call",
    { name, arguments: args },
    options,
  );
  return { status, body, result: body?.result?.structuredContent ?? null };
}

// Wire-level redaction pins: these must match the Rust structural pins.
const PROJECTION_KEYS = [
  "adaptive", "agentActive", "campaignId", "controlDisposition", "controlEpoch",
  "createdAt", "endedAt", "eventRange", "grant", "lastError", "lastOutcome",
  "observation", "ownerSessionId", "parentRunId", "progress", "runId",
  "startedAt", "state", "target", "terminal", "updatedAt", "version",
];
// Adaptive state is absent on a run with no adaptive authority, but the key is
// always present and null-valued, so it belongs in the pinned set above. When
// it is populated these are its exact keys — no challenge, digest, label,
// value, geometry, evidence token, or path may ever appear here.
const ADAPTIVE_KEYS = [
  "budget", "budgetExhausted", "ingestedAlias", "profile", "spend",
  "stationaryStrikes",
];
const OBSERVATION_KEYS = [
  "capturedAt", "elementCount", "elementsTruncated", "hasScreenshot",
  "observationId", "screenshotRedacted", "sensitivity", "sequence", "stale",
];
const LAST_OUTCOME_KEYS = ["expectedPostconditionMet"];
const LAST_ERROR_KEYS = ["code"];
const CAPACITY_KEYS = ["boundActiveRuns", "boundRuns", "maxRunRecords"];

function pinProjection(name, projection) {
  check(
    `${name}: projection keys are pinned`,
    JSON.stringify(Object.keys(projection).sort()) === JSON.stringify(PROJECTION_KEYS),
    Object.keys(projection).sort().join(","),
  );
  if (projection.adaptive) {
    check(
      `${name}: adaptive keys are pinned`,
      JSON.stringify(Object.keys(projection.adaptive).sort()) ===
        JSON.stringify(ADAPTIVE_KEYS),
      Object.keys(projection.adaptive).sort().join(","),
    );
    check(
      `${name}: adaptive profile is canonical, never an ingest alias`,
      ["economy", "balanced", "high_assurance"].includes(
        projection.adaptive.profile,
      ),
      String(projection.adaptive.profile),
    );
  }
  if (projection.observation) {
    check(
      `${name}: observation keys are pinned`,
      JSON.stringify(Object.keys(projection.observation).sort()) ===
        JSON.stringify(OBSERVATION_KEYS),
      Object.keys(projection.observation).sort().join(","),
    );
  }
  if (projection.lastOutcome) {
    check(
      `${name}: lastOutcome keys are pinned`,
      JSON.stringify(Object.keys(projection.lastOutcome).sort()) ===
        JSON.stringify(LAST_OUTCOME_KEYS),
      Object.keys(projection.lastOutcome).sort().join(","),
    );
  }
  if (projection.lastError) {
    check(
      `${name}: lastError keys are pinned`,
      JSON.stringify(Object.keys(projection.lastError).sort()) ===
        JSON.stringify(LAST_ERROR_KEYS),
      Object.keys(projection.lastError).sort().join(","),
    );
  }
}

async function main() {
  // Lifecycle: initialize + initialized.
  const init = await rpc(1, "initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "computer-reads-smoke", version: "1" },
  });
  const sid = init.headers.get("mcp-session-id");
  check("initialize succeeds with a transport session", init.status === 200 && !!sid);
  const initialized = await rpc(null, "notifications/initialized", {}, { session: sid });
  check("initialized notification is accepted", initialized.status === 202);

  // Discovery: the four read tools, and no computer mutation.
  const list = await rpc(2, "tools/list", {}, { session: sid });
  const names = (list.body?.result?.tools ?? []).map((t) => t.name);
  for (const name of [
    "ptah_list_computer_runs",
    "ptah_get_computer_run",
    "ptah_get_computer_run_events",
    "ptah_get_computer_capacity",
  ]) {
    check(`discovery includes ${name}`, names.includes(name));
  }
  check(
    "discovery has no computer mutation",
    !names.some(
      (n) =>
        n.includes("computer") &&
        !["ptah_list_computer_runs", "ptah_get_computer_run", "ptah_get_computer_run_events", "ptah_get_computer_capacity"].includes(n),
    ),
    names.join(","),
  );

  // Scoped listing: both bound runs of this session, never another session's.
  const listing = await tool(3, "ptah_list_computer_runs", callArgs(), { session: sid });
  const listedIds = (listing.result?.runs ?? []).map((r) => r.runId);
  check("listing succeeds", listing.status === 200);
  check("listing includes the observed run", listedIds.includes(runA));
  check("listing includes the long-journal run", listedIds.includes(runC));
  check("listing excludes another session's run", !listedIds.includes(runB));
  for (const projection of listing.result?.runs ?? []) {
    pinProjection(`list ${projection.runId}`, projection);
  }

  // Single-run projection: observation metadata without content.
  const got = await tool(4, "ptah_get_computer_run", callArgs({ run_id: runA }), { session: sid });
  check("get run succeeds", got.status === 200);
  check("run owner matches the claimed session", got.result?.ownerSessionId === sessionId);
  check(
    "observation metadata is present with counted elements",
    (got.result?.observation?.elementCount ?? 0) >= 1,
  );
  if (got.result) pinProjection("get", got.result);

  // Cursor paging: strictly increasing, bounded, terminating.
  const seqs = [];
  const operations = new Set();
  let cursor = null;
  let firstPage = null;
  for (let page = 0; page < 50; page += 1) {
    const args = callArgs({ run_id: runA, limit: 1 });
    if (cursor !== null) args.after_seq = cursor;
    const events = await tool(10 + page, "ptah_get_computer_run_events", args, { session: sid });
    check(`events page ${page} succeeds`, events.status === 200);
    if (firstPage === null) firstPage = events;
    for (const entry of events.result?.entries ?? []) {
      seqs.push(entry.sequence);
      operations.add(entry.operation);
    }
    check(`events page ${page} never reports expiry`, events.result?.cursorExpired === false);
    if (events.result?.nextCursor == null) break;
    cursor = events.result.nextCursor;
  }
  check(
    "event sequences are strictly increasing",
    seqs.every((seq, index) => index === 0 || seq > seqs[index - 1]),
    seqs.join(","),
  );
  for (const operation of ["create_run", "authorize", "observe"]) {
    check(`journal shows ${operation}`, operations.has(operation));
  }

  // Duplicate request: same id, same arguments — the identical body.
  const replay = await tool(10, "ptah_get_computer_run_events", callArgs({ run_id: runA, limit: 1 }), { session: sid });
  check(
    "read replay is byte-identical",
    JSON.stringify(replay.body) === JSON.stringify(firstPage.body),
  );

  // Beyond-tail cursor: a valid empty page, not an error, not expiry.
  const endSeq = got.result?.eventRange?.endSeq;
  const tail = await tool(60, "ptah_get_computer_run_events", callArgs({ run_id: runA, after_seq: endSeq }), { session: sid });
  check(
    "tail cursor returns an empty non-expired page",
    tail.status === 200 &&
      (tail.result?.entries ?? [1]).length === 0 &&
      tail.result?.cursorExpired === false,
  );

  // Stale cursor: below the retained ring is a hard 410. eventRange rides
  // the error so recovery does not require a second get.
  const stale = await tool(61, "ptah_get_computer_run_events", callArgs({ run_id: runC, after_seq: 0 }), { session: sid });
  check(
    "cursor below retention is 410 cursor_expired",
    stale.status === 410 && stale.body?.error?.data?.code === "cursor_expired",
    JSON.stringify(stale.body?.error ?? null),
  );
  const startSeq = stale.body?.error?.data?.eventRange?.startSeq;
  check("410 carries eventRange past the evicted prefix", (startSeq ?? 0) > 1, JSON.stringify(stale.body?.error ?? null));
  const resumed = await tool(63, "ptah_get_computer_run_events", callArgs({ run_id: runC, after_seq: startSeq - 1, limit: 5 }), { session: sid });
  check(
    "resuming from the retained start is continuity",
    resumed.status === 200 &&
      resumed.result?.cursorExpired === false &&
      resumed.result?.entries?.[0]?.sequence === startSeq,
  );

  // Cross-session and unknown runs are byte-identical failures.
  const crossSession = await tool(70, "ptah_get_computer_run", callArgs({ run_id: runB }), { session: sid });
  const unknown = await tool(70, "ptah_get_computer_run", callArgs({ run_id: "does-not-exist" }), { session: sid });
  check("cross-session read is refused", crossSession.status === 403);
  check(
    "cross-session and unknown runs are indistinguishable",
    JSON.stringify(crossSession.body) === JSON.stringify(unknown.body),
    JSON.stringify([crossSession.body, unknown.body]),
  );
  check(
    "refusal uses forbidden_scope",
    crossSession.body?.error?.data?.code === "forbidden_scope",
  );

  // Claiming a different allowlisted workspace, or an unknown session, is
  // the same unauthorized error — session existence is not an oracle.
  const crossWorkspace = await tool(70, "ptah_get_computer_run", { session_id: sessionId, workspace: otherWorkspace, run_id: runA }, { session: sid });
  const unknownSession = await tool(70, "ptah_get_computer_run", { session_id: "00000000-0000-4000-8000-000000000000", workspace, run_id: runA }, { session: sid });
  check(
    "cross-workspace is indistinguishable from unauthorized",
    JSON.stringify(crossWorkspace.body) === JSON.stringify(crossSession.body),
    JSON.stringify([crossWorkspace.body, crossSession.body]),
  );
  check(
    "unknown session is indistinguishable from unauthorized",
    JSON.stringify(unknownSession.body) === JSON.stringify(unknown.body),
    JSON.stringify([unknownSession.body, unknown.body]),
  );
  check(
    "cross-scope refusal uses forbidden_scope",
    crossWorkspace.body?.error?.data?.code === "forbidden_scope",
  );

  // Capacity is scoped to the (session, workspace) binding. Host-wide
  // occupancy would be a cross-scope activity oracle after the workspace gate.
  const capacity = await tool(72, "ptah_get_computer_capacity", callArgs(), { session: sid });
  check("capacity succeeds", capacity.status === 200);
  check(
    "capacity keys are pinned",
    JSON.stringify(Object.keys(capacity.result ?? {}).sort()) === JSON.stringify(CAPACITY_KEYS),
    Object.keys(capacity.result ?? {}).sort().join(","),
  );
  check("capacity max is the ledger bound", capacity.result?.maxRunRecords === 256);
  check("capacity counts only bound runs", capacity.result?.boundRuns === 2, JSON.stringify(capacity.result));
  check("capacity does not report host-global storedRuns", capacity.result?.storedRuns === undefined);

  // Auth-before-body on the new tools.
  const badToken = await rpc(80, "tools/call", { name: "ptah_list_computer_runs", arguments: callArgs() }, { token: "wrong-token", session: sid });
  check("wrong bearer is 401", badToken.status === 401);
  const malformed = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: '{"jsonrpc":"2.0","id":81,"method":"tools/call","params":{"name":"ptah_list_computer_runs"',
  });
  check("malformed unauthenticated body is 401 before parsing", malformed.status === 401);

  // Reconnect: end the MCP session, initialize a fresh one, and replay the
  // durable journal from the cursor captured before the reconnect.
  const del = await fetch(url, {
    method: "DELETE",
    headers: { Authorization: `Bearer ${token}`, "mcp-session-id": sid },
  });
  check("session delete is 204", del.status === 204);
  const reinit = await rpc(90, "initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "computer-reads-smoke", version: "1" },
  });
  const sid2 = reinit.headers.get("mcp-session-id");
  check("reconnect issues a new transport session", reinit.status === 200 && !!sid2 && sid2 !== sid);
  await rpc(null, "notifications/initialized", {}, { session: sid2 });
  const afterReconnect = await tool(91, "ptah_get_computer_run_events", callArgs({ run_id: runA, limit: 1 }), { session: sid2 });
  check(
    "durable events replay identically across reconnect",
    JSON.stringify(afterReconnect.result) === JSON.stringify(firstPage.result),
  );

  const report = {
    ok: failed.length === 0,
    checks: passed.length + failed.length,
    passed: passed.length,
    failed,
  };
  console.log(JSON.stringify(report));
  process.exit(report.ok ? 0 : 1);
}

main().catch((error) => {
  console.log(JSON.stringify({ ok: false, failed: [{ name: "unhandled", detail: String(error) }] }));
  process.exit(1);
});
