#!/usr/bin/env node
/**
 * Evidence-first continuity probe for the public Streamable HTTP contract.
 *
 * Every coordinator mutation is replayed with the same requestId. After the
 * first prompt, every prompt is a digest envelope derived from durable reads
 * and the harness-persisted chain state; no human continuation string is
 * supplied to a later run.
 */
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";

const url = process.env.GROKPTAH_MCP_URL;
const token = process.env.GROKPTAH_MCP_TOKEN;
const sessionId = process.env.GROKPTAH_MCP_SESSION_ID;
const gapSessionId = process.env.GROKPTAH_MCP_GAP_SESSION_ID;
const workspace = process.env.GROKPTAH_MCP_WORKSPACE;
const interruptedRunId = process.env.GROKPTAH_MCP_INTERRUPTED_RUN_ID;
const gapRunId = process.env.GROKPTAH_MCP_GAP_RUN_ID;
const gapReadyFile = process.env.GROKPTAH_MCP_GAP_READY_FILE;
const gapReleaseFile = process.env.GROKPTAH_MCP_GAP_RELEASE_FILE;
if (!url || !token || !sessionId || !gapSessionId || !workspace || !interruptedRunId || !gapRunId || !gapReadyFile || !gapReleaseFile) {
  console.error("continuity probe environment is incomplete");
  process.exit(2);
}

const endpoint = url.endsWith("/mcp") ? url : `${url.replace(/\/$/, "")}/mcp`;
const base = endpoint.replace(/\/mcp$/, "");
const prefix = `continuity-probe-${Date.now()}`;
const statePath = path.join(workspace, ".continuity-probe-state.json");
const checks = {};
const steps = [];
const mutationResults = [];
let requestNumber = 0;
let mcpSession = null;

function log(...args) {
  console.error("[continuity-probe]", ...args);
}

function record(name, ok, detail = null) {
  checks[name] = Boolean(ok);
  steps.push({ name, ok: Boolean(ok), detail });
  log(ok ? "ok" : "FAIL", name, detail ?? "");
}

function structured(json) {
  return json?.result?.structuredContent ?? json?.result ?? null;
}

function digest(value) {
  return crypto.createHash("sha256").update(JSON.stringify(value)).digest("hex");
}

async function mcpFetch(method, params, { id, session = mcpSession, notification = false } = {}) {
  const body = notification
    ? { jsonrpc: "2.0", method, params }
    : { jsonrpc: "2.0", id: id ?? ++requestNumber, method, params };
  const headers = {
    Authorization: `Bearer ${token}`,
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
    "MCP-Protocol-Version": "2025-11-25",
  };
  if (session) headers["mcp-session-id"] = session;
  const response = await fetch(endpoint, {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    // Preserve protocol failures as status/text for the final report.
  }
  return { status: response.status, sessionId: response.headers.get("mcp-session-id"), text, json };
}

async function call(name, args) {
  return mcpFetch("tools/call", {
    name,
    arguments: args,
  });
}

async function mutate(name, args) {
  const first = await call(name, args);
  const replay = await call(name, args);
  const firstValue = structured(first.json);
  const replayValue = structured(replay.json);
  const ok = first.status === 200 && replay.status === 200 &&
    JSON.stringify(firstValue) === JSON.stringify(replayValue);
  mutationResults.push({ name, requestId: args.request_id, ok });
  return { first, replay, value: firstValue, ok };
}

async function pollTerminal(runId, timeoutMs = 15_000) {
  const started = Date.now();
  let last = null;
  while (Date.now() - started < timeoutMs) {
    const response = await call("ptah_get_run", {
      session_id: sessionId,
      workspace,
      run_id: runId,
    });
    last = structured(response.json);
    if (["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(last?.state)) {
      return last;
    }
    await new Promise((resolve) => setTimeout(resolve, 60));
  }
  return last;
}

async function durableReads(runId) {
  const [run, changes, tests, handoff] = await Promise.all([
    call("ptah_get_run", { session_id: sessionId, workspace, run_id: runId }),
    call("ptah_get_changes", { session_id: sessionId, workspace, run_id: runId }),
    call("ptah_get_test_results", { session_id: sessionId, workspace, run_id: runId }),
    call("ptah_get_handoff", { session_id: sessionId, workspace, run_id: runId }),
  ]);
  return {
    run: structured(run.json),
    changes: structured(changes.json),
    tests: structured(tests.json),
    handoff: structured(handoff.json),
    statuses: [run, changes, tests, handoff].map((item) => item.status),
  };
}

function persistState(state) {
  fs.writeFileSync(statePath, `${JSON.stringify(state, null, 2)}\n`);
}

function derivedPrompt(state, ordinal, parentRunId, durable, extra = {}) {
  const derivationInputs = {
    chainId: state.chainId,
    ordinal,
    parentRunId,
    durable,
    chainState: {
      chainId: state.chainId,
      ordinal: state.ordinal,
      parentRunId: state.parentRunId,
      history: state.history.map((entry) => ({
        ordinal: entry.ordinal,
        label: entry.label,
        runId: entry.runId,
        parentRunId: entry.parentRunId,
        derivationHash: entry.derivationHash,
        terminalState: entry.terminalState,
        retryOf: entry.retryOf,
      })),
    },
    ...extra,
  };
  const derivationHash = digest(derivationInputs);
  const prompt = JSON.stringify({
    chainId: state.chainId,
    ordinal,
    parentRunId,
    derivationHash,
  });
  return { prompt, derivationHash, derivationInputs };
}

function appendHistory(state, entry) {
  state.history.push(entry);
  state.ordinal = entry.ordinal;
  state.parentRunId = entry.runId;
  persistState(state);
}

async function runDerived(state, parentRunId, label, extra = {}) {
  const durable = await durableReads(parentRunId);
  const ordinal = state.ordinal + 1;
  const derivation = derivedPrompt(state, ordinal, parentRunId, durable, extra);
  const rechecked = digest(derivation.derivationInputs) === derivation.derivationHash;
  const args = {
    request_id: `${prefix}-${label}-submit`,
    session_id: sessionId,
    workspace,
    prompt: derivation.prompt,
    execution_mode: "shared",
    bounds: { maxPromptBytes: 10_000, maxRounds: 8, maxDurationMs: 30_000 },
  };
  const submitted = await mutate("ptah_submit_task", args);
  const runId = submitted.value?.runId;
  const terminal = runId ? await pollTerminal(runId) : null;
  appendHistory(state, {
    ordinal,
    label,
    runId,
    parentRunId,
    derivationHash: derivation.derivationHash,
    derivationInputs: derivation.derivationInputs,
    prompt: derivation.prompt,
    terminalState: terminal?.state ?? null,
    retryOf: extra.retryOf ?? null,
  });
  return { runId, terminal, submitted, rechecked };
}

async function openLive(runId, lastEventId = null, ownerSession = sessionId) {
  const liveUrl = new URL(endpoint);
  liveUrl.searchParams.set("session_id", ownerSession);
  liveUrl.searchParams.set("workspace", workspace);
  liveUrl.searchParams.set("run_id", runId);
  const headers = {
    Authorization: `Bearer ${token}`,
    Accept: "text/event-stream",
    "MCP-Protocol-Version": "2025-11-25",
    "mcp-session-id": mcpSession,
  };
  if (lastEventId != null) headers["Last-Event-ID"] = String(lastEventId);
  return fetch(liveUrl, { headers });
}

async function nextFrame(reader, pending = "") {
  let buffer = pending;
  while (true) {
    const split = buffer.indexOf("\n\n");
    if (split >= 0) {
      const frame = buffer.slice(0, split);
      buffer = buffer.slice(split + 2);
      const data = frame.split(/\r?\n/).find((line) => line.startsWith("data: "))?.slice(6);
      if (data) {
        return {
          frame,
          buffer,
          id: Number(frame.match(/(?:^|\n)id: (\d+)/)?.[1] ?? 0),
          body: JSON.parse(data),
        };
      }
    }
    const next = await reader.read();
    if (next.done) throw new Error("SSE stream closed before the expected frame");
    buffer += new TextDecoder().decode(next.value);
  }
}

async function waitForRecovery(reader) {
  let buffer = "";
  let frames = 0;
  while (frames++ < 20_000) {
    const next = await nextFrame(reader, buffer);
    buffer = next.buffer;
    if (next.body?.method === "notifications/ptah_recovery") return next;
  }
  throw new Error("SSE stream did not surface a recovery notification");
}

async function waitForRelease() {
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    if (fs.existsSync(gapReleaseFile)) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error("continuity test driver did not release the held SSE stream");
}

const watchdog = setTimeout(() => {
  log("watchdog timeout");
  console.log(JSON.stringify({ ok: false, reason: "watchdog", checks, steps }));
  process.exit(3);
}, 120_000);

try {
  const init = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "grokptah-continuity-probe", version: "1.0.0" },
  });
  mcpSession = init.sessionId;
  const initialized = await mcpFetch("notifications/initialized", {}, { notification: true });
  record("mcpSessionReady", init.status === 200 && initialized.status >= 200 && initialized.status < 300);

  const firstPrompt = "write continuity-probe-seed.txt: first continuity probe marker";
  const firstSubmit = await mutate("ptah_submit_task", {
    request_id: `${prefix}-first-submit`,
    session_id: sessionId,
    workspace,
    prompt: firstPrompt,
    execution_mode: "isolated_worktree",
    bounds: { maxPromptBytes: 10_000, maxRounds: 8, maxDurationMs: 30_000 },
  });
  const firstRunId = firstSubmit.value?.runId;
  const firstTerminal = firstRunId ? await pollTerminal(firstRunId) : null;
  const firstBeforeReview = firstRunId
    ? structured((await call("ptah_get_run", { session_id: sessionId, workspace, run_id: firstRunId })).json)
    : null;
  const sourceFile = path.join(workspace, "continuity-probe-seed.txt");
  record(
    "isolatedReadyBeforeReview",
    firstSubmit.ok && firstTerminal?.state === "completed" &&
      firstBeforeReview?.execution?.promotionState === "ready" && !fs.existsSync(sourceFile),
    { runId: firstRunId, state: firstTerminal?.state, promotionState: firstBeforeReview?.execution?.promotionState }
  );

  const reviewResponse = firstRunId
    ? await call("ptah_review_run", { session_id: sessionId, workspace, run_id: firstRunId })
    : { status: 0, json: null };
  const review = structured(reviewResponse.json);
  const noAutoPromotion = firstBeforeReview?.execution?.promotionState === "ready" && !fs.existsSync(sourceFile);
  const approvalArgs = firstRunId && review
    ? {
        request_id: `${prefix}-first-approve`,
        session_id: sessionId,
        workspace,
        run_id: firstRunId,
        source_fingerprint: review.sourceFingerprint,
        final_fingerprint: review.finalFingerprint,
        changed_files: review.changedFiles,
        ttl_ms: 300_000,
      }
    : null;
  const approval = approvalArgs ? await mutate("ptah_approve_run", approvalArgs) : { ok: false, value: null };
  const promoteArgs = approval.value?.approvalId
    ? {
        request_id: `${prefix}-first-promote`,
        session_id: sessionId,
        workspace,
        run_id: firstRunId,
        approval_id: approval.value.approvalId,
      }
    : null;
  const promoted = promoteArgs ? await mutate("ptah_promote_run", promoteArgs) : { ok: false, value: null };
  record(
    "explicitReviewApprovePromote",
    reviewResponse.status === 200 && approval.ok && promoted.ok && fs.existsSync(sourceFile) &&
      fs.readFileSync(sourceFile, "utf8").includes("first continuity probe marker"),
    { runId: firstRunId, reviewStatus: reviewResponse.status, promoted: promoted.value?.execution?.promotionState }
  );
  record("noAutoPromotion", noAutoPromotion);

  const state = {
    chainId: `${prefix}-chain`,
    ordinal: 1,
    parentRunId: firstRunId,
    history: [{
      ordinal: 1,
      label: "first-isolated",
      runId: firstRunId,
      parentRunId: null,
      derivationHash: null,
      derivationInputs: null,
      prompt: firstPrompt,
      terminalState: firstTerminal?.state ?? null,
      retryOf: null,
    }],
  };
  persistState(state);

  const second = await runDerived(state, firstRunId, "second");
  const third = await runDerived(state, second.runId, "third");
  const fourth = await runDerived(state, third.runId, "fourth");

  const interruptedDurable = await durableReads(interruptedRunId);
  const interrupted = structured((await call("ptah_get_run", {
    session_id: sessionId,
    workspace,
    run_id: interruptedRunId,
  })).json);
  const retryOrdinal = state.ordinal + 1;
  const retryDerivation = derivedPrompt(
    state,
    retryOrdinal,
    state.parentRunId,
    interruptedDurable,
    { interruptionSourceRunId: interruptedRunId }
  );
  const retry = await mutate("ptah_retry_run", {
    request_id: `${prefix}-restart-retry`,
    session_id: sessionId,
    workspace,
    run_id: interruptedRunId,
    prompt: retryDerivation.prompt,
  });
  const retryRunId = retry.value?.runId;
  const retryTerminal = retryRunId ? await pollTerminal(retryRunId) : null;
  appendHistory(state, {
    ordinal: retryOrdinal,
    label: "explicit-restart-retry",
    runId: retryRunId,
    parentRunId: state.parentRunId,
    derivationHash: retryDerivation.derivationHash,
    derivationInputs: retryDerivation.derivationInputs,
    prompt: retryDerivation.prompt,
    terminalState: retryTerminal?.state ?? null,
    retryOf: interruptedRunId,
  });
  record(
    "explicitInterruptedTerminal",
    interrupted?.state === "interrupted" && interrupted?.terminalResult === "interrupted",
    { runId: interruptedRunId, state: interrupted?.state }
  );
  record(
    "retryCreatesFreshRun",
    retry.ok && retry.value?.sourceRunId === interruptedRunId && retry.value?.retryOf === interruptedRunId &&
      retryRunId && retryRunId !== interruptedRunId && ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(retryTerminal?.state),
    { sourceRunId: interruptedRunId, retryRunId, state: retryTerminal?.state }
  );

  const fresh = await runDerived(state, retryRunId, "fresh-after-retry", { retryOf: interruptedRunId });
  const freshRun = fresh.runId ? structured((await call("ptah_get_run", {
    session_id: sessionId,
    workspace,
    run_id: fresh.runId,
  })).json) : null;
  record(
    "freshSubmitAfterRetry",
    fresh.submitted.ok && fresh.runId && fresh.runId !== retryRunId && freshRun?.retryOf == null &&
      ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(fresh.terminal?.state),
    { retryRunId, freshRunId: fresh.runId, state: fresh.terminal?.state }
  );

  const savedState = JSON.parse(fs.readFileSync(statePath, "utf8"));
  const allLinks = savedState.history.length >= 5 &&
    savedState.history.every((entry, index) => {
      if (typeof entry.runId !== "string") return false;
      if (index === 0) return entry.parentRunId == null;
      return entry.parentRunId === savedState.history[index - 1].runId;
    }) &&
    new Set(savedState.history.map((entry) => entry.runId)).size === savedState.history.length &&
    savedState.history.some((entry) => entry.retryOf === interruptedRunId);
  const allHashes = savedState.history.slice(1).every((entry) =>
    typeof entry.derivationHash === "string" && digest(entry.derivationInputs) === entry.derivationHash &&
    JSON.parse(entry.prompt).derivationHash === entry.derivationHash
  );
  record("chainHasFiveRuns", savedState.history.length >= 5, { count: savedState.history.length });
  record("durableDerivationChain", allLinks, savedState.history.map((entry) => ({ ordinal: entry.ordinal, runId: entry.runId, parentRunId: entry.parentRunId })));
  record("promptHashesRechecked", allHashes, savedState.history.slice(1).map((entry) => entry.derivationHash));

  // Drop the first response after one durable event, then reconnect with its
  // Last-Event-ID. This is the ordinary loss/reconnect path.
  const firstStream = await openLive(firstRunId);
  const firstReader = firstStream.body.getReader();
  const firstFrame = await nextFrame(firstReader);
  await firstReader.cancel();
  const resumedStream = await openLive(firstRunId, firstFrame.id);
  const resumedReader = resumedStream.body.getReader();
  const resumedFrame = await nextFrame(resumedReader);
  await resumedReader.cancel();
  record(
    "dropReconnectsByLastEventId",
    firstStream.status === 200 && resumedStream.status === 200 &&
      firstFrame.body?.method === "notifications/ptah_event" &&
      resumedFrame.body?.method === "notifications/ptah_event" && resumedFrame.id > firstFrame.id,
    { firstId: firstFrame.id, resumedId: resumedFrame.id }
  );

  // Hold the live body unread while the Rust driver floods the bounded
  // subscriber. The server must emit a recovery notification, after which
  // the coordinator reconstructs state with the durable read tool.
  log("opening gap stream");
  const gapStream = await openLive(gapRunId, null, gapSessionId);
  if (gapStream.status !== 200) throw new Error(`gap stream HTTP ${gapStream.status}`);
  log("gap stream opened");
  fs.writeFileSync(gapReadyFile, "ready");
  await waitForRelease();
  log("release observed; waiting for recovery");
  const recovery = await waitForRecovery(gapStream.body.getReader());
  log("recovery observed");
  const gapRead = await call("ptah_get_events", {
    session_id: gapSessionId,
    workspace,
    run_id: gapRunId,
    after_seq: 0,
    limit: 500,
  });
  const gapPage = structured(gapRead.json);
  record(
    "gapNoticeObserved",
    recovery.body?.method === "notifications/ptah_recovery" && recovery.body?.params?.pollTool === "ptah_get_events",
    recovery.body?.params
  );
  record(
    "durableReadReconcilesGap",
    gapRead.status === 200 && gapPage?.cursorExpired === false && Array.isArray(gapPage?.entries) && gapPage.entries.length > 0,
    { status: gapRead.status, entries: gapPage?.entries?.length, cursorExpired: gapPage?.cursorExpired }
  );

  record("mutationsReplayByRequestId", mutationResults.length >= 8 && mutationResults.every((item) => item.ok), mutationResults);
  clearTimeout(watchdog);
  const failed = Object.entries(checks).filter(([, ok]) => !ok).map(([name]) => name);
  console.log(JSON.stringify({
    ok: failed.length === 0,
    failed,
    checks,
    steps,
    mutationResults,
    chainStatePath: statePath,
    chainRuns: savedState.history.map((entry) => ({ ordinal: entry.ordinal, runId: entry.runId, parentRunId: entry.parentRunId, retryOf: entry.retryOf })),
    coordinator: "continuity-probe-fetch-mcp-streamable-http",
  }));
  process.exit(failed.length === 0 ? 0 : 1);
} catch (error) {
  clearTimeout(watchdog);
  console.error(error);
  console.log(JSON.stringify({ ok: false, error: String(error), checks, steps, mutationResults }));
  process.exit(1);
}
