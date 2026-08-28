#!/usr/bin/env node
/**
 * Reference coordinator campaign for GrokPtah's Streamable HTTP control plane.
 *
 * This is deliberately protocol-level fetch rather than an in-tree Rust
 * client. A coordinator can run the same campaign against a desktop bootstrap
 * or the deterministic in-process server used by the integration test.
 *
 * Required environment:
 *   GROKPTAH_MCP_URL       http://127.0.0.1:PORT[/mcp]
 *   GROKPTAH_MCP_TOKEN     bearer token
 *   GROKPTAH_MCP_SESSION_ID Build session UUID on that host
 *   GROKPTAH_MCP_OTHER_SESSION_ID second Build session UUID on that host
 *   GROKPTAH_MCP_DISCARD_SESSION_ID Build session for isolated discard
 *   GROKPTAH_MCP_WORKSPACE  disposable, allowlisted Git workspace
 *   GROKPTAH_MCP_DISCARD_WORKSPACE second disposable, allowlisted workspace
 */
import fs from "node:fs";
import path from "node:path";

const url = process.env.GROKPTAH_MCP_URL;
const token = process.env.GROKPTAH_MCP_TOKEN;
const hostSessionId = process.env.GROKPTAH_MCP_SESSION_ID;
const otherSessionId = process.env.GROKPTAH_MCP_OTHER_SESSION_ID;
const discardSessionId = process.env.GROKPTAH_MCP_DISCARD_SESSION_ID;
const workspace = process.env.GROKPTAH_MCP_WORKSPACE;
const discardWorkspace = process.env.GROKPTAH_MCP_DISCARD_WORKSPACE;
const interruptedRunId = process.env.GROKPTAH_MCP_INTERRUPTED_RUN_ID;
if (!url || !token || !hostSessionId || !otherSessionId || !discardSessionId || !workspace || !discardWorkspace) {
  console.error(
    "GROKPTAH_MCP_URL, GROKPTAH_MCP_TOKEN, GROKPTAH_MCP_SESSION_ID, GROKPTAH_MCP_OTHER_SESSION_ID, GROKPTAH_MCP_DISCARD_SESSION_ID, GROKPTAH_MCP_WORKSPACE, GROKPTAH_MCP_DISCARD_WORKSPACE required"
  );
  process.exit(2);
}

const endpoint = url.endsWith("/mcp") ? url : `${url.replace(/\/$/, "")}/mcp`;
const base = endpoint.replace(/\/mcp$/, "");
const prefix = `coordinator-campaign-${Date.now()}`;
const checks = {};
const steps = [];
let requestNumber = 0;
let mcpSession = null;
const scopedReadTools = new Set([
  "ptah_get_run",
  "ptah_get_progress",
  "ptah_get_events",
  "ptah_get_changes",
  "ptah_get_test_results",
  "ptah_get_handoff",
  "ptah_review_run",
]);

function log(...args) {
  console.error("[coordinator-campaign]", ...args);
}

function record(name, ok, detail = null) {
  checks[name] = Boolean(ok);
  steps.push({ name, ok: Boolean(ok), detail });
  log(ok ? "ok" : "FAIL", name, detail ?? "");
}

function structured(json) {
  return json?.result?.structuredContent ?? json?.result ?? null;
}

async function mcpFetch(method, params, { id, sessionId = mcpSession, notification = false } = {}) {
  const body = notification
    ? { jsonrpc: "2.0", method, params }
    : { jsonrpc: "2.0", id: id ?? ++requestNumber, method, params };
  const headers = {
    Authorization: `Bearer ${token}`,
    "Content-Type": "application/json",
    Accept: "application/json, text/event-stream",
    "MCP-Protocol-Version": "2025-11-25",
  };
  if (sessionId) headers["mcp-session-id"] = sessionId;
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
    // A protocol failure is represented by status/text in the report.
  }
  return {
    status: response.status,
    sessionId: response.headers.get("mcp-session-id"),
    text,
    json,
  };
}

async function call(name, args, id) {
  const scopedArgs =
    scopedReadTools.has(name) && args?.run_id && !args.session_id
      ? { ...args, session_id: hostSessionId, workspace }
      : args;
  return mcpFetch(
    "tools/call",
    { name, arguments: scopedArgs },
    { id: id ?? ++requestNumber }
  );
}

async function pollTerminal(
  runId,
  timeoutMs = 15_000,
  scope = { session_id: hostSessionId, workspace }
) {
  const started = Date.now();
  let last = null;
  while (Date.now() - started < timeoutMs) {
    const response = await call("ptah_get_run", { ...scope, run_id: runId });
    last = structured(response.json);
    if (
      ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(
        last?.state
      )
    ) {
      return last;
    }
    await new Promise((resolve) => setTimeout(resolve, 60));
  }
  return last;
}

function changedFilesAreBounded(review) {
  return (
    review &&
    typeof review.runId === "string" &&
    typeof review.sourceFingerprint === "string" &&
    typeof review.finalFingerprint === "string" &&
    Array.isArray(review.changedFiles) &&
    review.changedFiles.length > 0 &&
    review.changedFiles.every(
      (file) =>
        file &&
        typeof file.path === "string" &&
        typeof file.summary === "string" &&
        !path.isAbsolute(file.path) &&
        !file.path.split(/[\\/]/).includes("..")
    )
  );
}

const watchdog = setTimeout(() => {
  log("watchdog timeout");
  console.log(JSON.stringify({ ok: false, reason: "watchdog", checks, steps }));
  process.exit(3);
}, 60_000);

try {
  const health = await fetch(`${base}/health`);
  const healthJson = await health.json();
  record(
    "healthAndCapacity",
    health.status === 200 &&
      healthJson.ok === true &&
      healthJson.status === "alive" &&
      healthJson.authoritative === false &&
      healthJson.capacity === undefined,
    healthJson
  );

  const init = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "grokptah-reference-coordinator", version: "1.0.0" },
  });
  mcpSession = init.sessionId;
  record("initialize", init.status === 200 && !!mcpSession, init.sessionId);
  const initialized = await mcpFetch("notifications/initialized", {}, { notification: true });
  record("initializedNotification", initialized.status === 202 || initialized.status === 200);

  const listed = await mcpFetch("tools/list", {});
  const tools = listed.json?.result?.tools ?? [];
  const names = tools.map((tool) => tool.name);
  record(
    "boundedToolCatalog",
    listed.status === 200 &&
      [
        "ptah_list_sessions",
        "ptah_get_capacity",
        "ptah_get_events",
        "ptah_submit_task",
        "ptah_retry_run",
        "ptah_queue_prompt",
        "ptah_steer",
        "ptah_cancel",
        "ptah_review_run",
        "ptah_approve_run",
        "ptah_promote_run",
        "ptah_discard_run",
      ].every((name) => names.includes(name)) &&
      !names.some((name) => /shell|terminal|manage_plugin|manage_mcp/i.test(name)),
    { count: names.length, names }
  );

  const sessions = await call("ptah_list_sessions", {});
  const sessionList = structured(sessions.json)?.sessions ?? [];
  record(
    "sessionOwnership",
    sessions.status === 200 &&
      sessionList.some((item) => String(item.sessionId) === String(hostSessionId)),
    { hostSessionId, count: sessionList.length }
  );
  const capacity = await call("ptah_get_capacity", {});
  record("capacityRead", capacity.status === 200 && !!structured(capacity.json), structured(capacity.json));

  // Session cwd and workspace allowlist are independent authorization
  // boundaries. A coordinator must not steer through a sibling path merely
  // because it is on the same filesystem.
  const outsideWorkspace = path.resolve(workspace, "..");
  const workspaceMismatch = await call("ptah_queue_prompt", {
    request_id: `${prefix}-workspace-mismatch`,
    session_id: hostSessionId,
    workspace: outsideWorkspace,
    prompt: "this must be rejected outside the allowlist",
  });
  record("workspaceMismatchFailClosed", workspaceMismatch.status >= 400, {
    status: workspaceMismatch.status,
    workspace: outsideWorkspace,
  });

  // A shared run proves ordinary coordinator submission and durable reads.
  const sharedSubmit = await call("ptah_submit_task", {
    request_id: `${prefix}-shared-submit`,
    session_id: hostSessionId,
    workspace,
    prompt: "list files in the project root",
    execution_mode: "shared",
    bounds: { maxPromptBytes: 10_000, maxRounds: 6, maxDurationMs: 30_000 },
  });
  const shared = structured(sharedSubmit.json);
  const sharedRunId = shared?.runId;
  record(
    "sharedSubmit",
    sharedSubmit.status === 200 && typeof sharedRunId === "string" && shared.executionMode === "shared",
    shared
  );
  const sharedTerminal = sharedRunId ? await pollTerminal(sharedRunId) : null;
  record(
    "sharedTerminal",
    ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(sharedTerminal?.state),
    sharedTerminal?.state
  );
  if (sharedRunId) {
    const missingReadScope = await mcpFetch("tools/call", {
      name: "ptah_get_run",
      arguments: { run_id: sharedRunId },
    });
    record("readMissingScopeFailClosed", missingReadScope.status >= 400, {
      status: missingReadScope.status,
    });
    const foreignReadScope = await call("ptah_get_run", {
      session_id: otherSessionId,
      workspace,
      run_id: sharedRunId,
    });
    record("readCrossSessionFailClosed", foreignReadScope.status >= 400, {
      status: foreignReadScope.status,
    });
    const wrongWorkspaceRead = await call("ptah_get_run", {
      session_id: hostSessionId,
      workspace: discardWorkspace,
      run_id: sharedRunId,
    });
    record("readCrossWorkspaceFailClosed", wrongWorkspaceRead.status >= 400, {
      status: wrongWorkspaceRead.status,
    });

    const sharedReplay = await call("ptah_submit_task", {
      request_id: `${prefix}-shared-submit`,
      session_id: hostSessionId,
      workspace,
      prompt: "list files in the project root",
      execution_mode: "shared",
      bounds: { maxPromptBytes: 10_000, maxRounds: 6, maxDurationMs: 30_000 },
    });
    record("submitIdempotency", sharedReplay.status === 200 && structured(sharedReplay.json)?.runId === sharedRunId);

    const progress = await call("ptah_get_progress", { run_id: sharedRunId });
    const changes = await call("ptah_get_changes", { run_id: sharedRunId });
    const tests = await call("ptah_get_test_results", { run_id: sharedRunId });
    const handoff = await call("ptah_get_handoff", { run_id: sharedRunId });
    record("durableReadParity", [progress, changes, tests, handoff].every((item) => item.status === 200), {
      progress: progress.status,
      changes: changes.status,
      tests: tests.status,
      handoff: handoff.status,
    });

    let after = 0;
    let previous = 0;
    const sequences = [];
    let pages = 0;
    let cursorExpired = false;
    while (pages++ < 32) {
      const pageResponse = await call("ptah_get_events", {
        run_id: sharedRunId,
        after_seq: after,
        limit: 3,
      });
      const page = structured(pageResponse.json);
      cursorExpired = page?.cursorExpired === true;
      if (cursorExpired || pageResponse.status !== 200) break;
      for (const entry of page?.entries ?? []) {
        if (typeof entry.seq !== "number" || entry.seq <= previous) {
          cursorExpired = true;
          break;
        }
        previous = entry.seq;
        sequences.push(entry.seq);
      }
      const next = page?.nextCursor;
      if (cursorExpired || next == null || next === after) break;
      after = next;
    }
    record("cursorReplayNoGaps", !cursorExpired && sequences.length > 0, {
      pages,
      entries: sequences.length,
      first: sequences[0],
      last: sequences.at(-1),
    });
  }

  // Restart recovery is explicit: the source was durably marked interrupted
  // before this coordinator connected, so no model turn can resume silently.
  if (interruptedRunId) {
    const retryArgs = {
      request_id: `${prefix}-retry-interrupted`,
      session_id: hostSessionId,
      workspace,
      run_id: interruptedRunId,
      prompt: "continue with a fresh bounded recovery prompt",
    };
    const retry = await call("ptah_retry_run", retryArgs);
    const retryState = structured(retry.json);
    const retryRunId = retryState?.runId;
    const retryTerminal = retryRunId ? await pollTerminal(retryRunId) : null;
    const replay = await call("ptah_retry_run", retryArgs);
    const conflict = await call("ptah_retry_run", {
      ...retryArgs,
      prompt: "a different retry payload must conflict",
    });
    record(
      "explicitRestartRetry",
      retry.status === 200 &&
        retryState?.sourceRunId === interruptedRunId &&
        retryState?.retryOf === interruptedRunId &&
        typeof retryRunId === "string" &&
        ["completed", "failed", "cancelled", "interrupted", "limit_reached"].includes(
          retryTerminal?.state
        ) &&
        replay.status === 200 &&
        JSON.stringify(structured(replay.json)) === JSON.stringify(retryState) &&
        (conflict.status >= 400 || !!conflict.json?.error),
      {
        sourceRunId: interruptedRunId,
        retryRunId,
        state: retryTerminal?.state,
        replayStatus: replay.status,
        conflictStatus: conflict.status,
      }
    );
  }

  // A busy run accepts steering without being cancelled, then responds to an
  // explicit cancellation request. This is the coordinator's main race path.
  const busySubmit = await call("ptah_submit_task", {
    request_id: `${prefix}-busy-submit`,
    session_id: hostSessionId,
    workspace,
    prompt: "run sleep 2",
    bounds: { maxPromptBytes: 10_000, maxRounds: 8, maxDurationMs: 30_000 },
  });
  const busy = structured(busySubmit.json);
  const busyRunId = busy?.runId;
  record("busySubmit", busySubmit.status === 200 && typeof busyRunId === "string", busy);
  if (busyRunId) {
    await new Promise((resolve) => setTimeout(resolve, 100));
    const steer = await call("ptah_steer", {
      request_id: `${prefix}-busy-steer`,
      session_id: hostSessionId,
      workspace,
      text: "finish the current turn without starting another turn",
    });
    const steerState = structured(steer.json);
    const midRun = await call("ptah_get_run", { run_id: busyRunId });
    const midState = structured(midRun.json)?.state;
    record(
      "busySteerNonCancelling",
      steer.status === 200 && ["pending", "queued"].includes(steerState?.disposition) &&
        midState !== "cancelled",
      { disposition: steerState?.disposition, state: midState }
    );
    const crossSessionCancel = await call("ptah_cancel", {
      request_id: `${prefix}-cross-session-cancel`,
      session_id: otherSessionId,
      workspace,
      run_id: busyRunId,
    });
    record(
      "crossSessionRunOwnershipFailClosed",
      crossSessionCancel.status >= 400,
      { status: crossSessionCancel.status }
    );
    const crossWorkspaceCancel = await call("ptah_cancel", {
      request_id: `${prefix}-cross-workspace-cancel`,
      session_id: hostSessionId,
      workspace: outsideWorkspace,
      run_id: busyRunId,
    });
    record(
      "crossWorkspaceRunOwnershipFailClosed",
      crossWorkspaceCancel.status >= 400,
      { status: crossWorkspaceCancel.status }
    );
    const cancel = await call("ptah_cancel", {
      request_id: `${prefix}-busy-cancel`,
      session_id: hostSessionId,
      workspace,
      run_id: busyRunId,
    });
    const cancelled = await pollTerminal(busyRunId);
    record(
      "explicitCancel",
      cancel.status === 200 && structured(cancel.json)?.cancelled === true &&
        cancelled?.state === "cancelled",
      { cancel: structured(cancel.json), finalState: cancelled?.state }
    );
  }

  // Isolated execution proves bounded diff review, exact approval scope, and promotion.
  const isolatedFile = path.join(workspace, "coordinator-campaign.txt");
  const isolatedSubmit = await call("ptah_submit_task", {
    request_id: `${prefix}-isolated-submit`,
    session_id: hostSessionId,
    workspace,
    prompt: "write coordinator-campaign.txt: created by the reference coordinator",
    execution_mode: "isolated_worktree",
    bounds: { maxPromptBytes: 10_000, maxRounds: 8, maxDurationMs: 30_000 },
  });
  const isolated = structured(isolatedSubmit.json);
  const isolatedRunId = isolated?.runId;
  const isolatedTerminal = isolatedRunId ? await pollTerminal(isolatedRunId) : null;
  record(
    "isolatedRun",
    isolatedSubmit.status === 200 && isolatedTerminal?.state === "completed" && isolated.executionMode === "isolated_worktree",
    { runId: isolatedRunId, state: isolatedTerminal?.state }
  );
  if (isolatedRunId) {
    const reviewResponse = await call("ptah_review_run", { run_id: isolatedRunId });
    const review = structured(reviewResponse.json);
    record("boundedDiffReview", reviewResponse.status === 200 && changedFilesAreBounded(review), review);
    if (reviewResponse.status === 200 && changedFilesAreBounded(review)) {
      const staleApprovalResponse = await call("ptah_approve_run", {
        request_id: `${prefix}-isolated-stale-approve`,
        session_id: hostSessionId,
        workspace,
        run_id: isolatedRunId,
        source_fingerprint: review.sourceFingerprint,
        final_fingerprint: review.finalFingerprint,
        changed_files: review.changedFiles,
        ttl_ms: 1,
      });
      const staleApproval = structured(staleApprovalResponse.json);
      await new Promise((resolve) => setTimeout(resolve, 80));
      const stalePromotion = await call("ptah_promote_run", {
        request_id: `${prefix}-isolated-stale-promote`,
        session_id: hostSessionId,
        workspace,
        run_id: isolatedRunId,
        approval_id: staleApproval?.approvalId,
      });
      record(
        "staleApprovalFailClosed",
        staleApprovalResponse.status === 200 && stalePromotion.status >= 400,
        { approvalStatus: staleApprovalResponse.status, promoteStatus: stalePromotion.status }
      );
      const scopeConflict = await call("ptah_promote_run", {
        request_id: `${prefix}-isolated-wrong-approval`,
        session_id: hostSessionId,
        workspace,
        run_id: isolatedRunId,
        approval_id: "approval-for-a-different-run",
      });
      record("approvalScopeConflictFailClosed", scopeConflict.status >= 400, {
        status: scopeConflict.status,
      });

      const approvalResponse = await call("ptah_approve_run", {
        request_id: `${prefix}-isolated-approve`,
        session_id: hostSessionId,
        workspace,
        run_id: isolatedRunId,
        source_fingerprint: review.sourceFingerprint,
        final_fingerprint: review.finalFingerprint,
        changed_files: review.changedFiles,
        ttl_ms: 300_000,
      });
      const approval = structured(approvalResponse.json);
      record("scopedApproval", approvalResponse.status === 200 && typeof approval?.approvalId === "string", approval);
      if (approval?.approvalId) {
        const promote = await call("ptah_promote_run", {
          request_id: `${prefix}-isolated-promote`,
          session_id: hostSessionId,
          workspace,
          run_id: isolatedRunId,
          approval_id: approval.approvalId,
        });
        const promoted = structured(promote.json);
        record(
          "scopedPromotion",
          promote.status === 200 && promoted?.execution?.promotionState === "promoted" &&
            fs.existsSync(isolatedFile) &&
            fs.readFileSync(isolatedFile, "utf8").includes("reference coordinator"),
          promoted
        );
        // The campaign owns this disposable promoted fixture. Restore the
        // clean baseline before the independent discard case so its isolated
        // precondition tests the platform rather than our prior artifact.
        if (promote.status === 200 && fs.existsSync(isolatedFile)) {
          fs.unlinkSync(isolatedFile);
        }
      }
    }
  }

  const discardFile = path.join(discardWorkspace, "coordinator-discard.txt");
  const discardSubmit = await call("ptah_submit_task", {
    request_id: `${prefix}-discard-submit`,
    session_id: discardSessionId,
    workspace: discardWorkspace,
    prompt: "write coordinator-discard.txt: this isolated result must be discarded",
    execution_mode: "isolated_worktree",
    bounds: { maxPromptBytes: 10_000, maxRounds: 8, maxDurationMs: 30_000 },
  });
  const discardRun = structured(discardSubmit.json);
  const discardRunId = discardRun?.runId;
  const discardTerminal = discardRunId
    ? await pollTerminal(discardRunId, 15_000, {
        session_id: discardSessionId,
        workspace: discardWorkspace,
      })
    : null;
  const discardResponse = discardRunId
    ? await call("ptah_discard_run", {
        request_id: `${prefix}-discard-run`,
        session_id: discardSessionId,
        workspace: discardWorkspace,
        run_id: discardRunId,
      })
    : { status: 0, json: null };
  const discardedState = discardRunId
    ? await call("ptah_get_run", {
        session_id: discardSessionId,
        workspace: discardWorkspace,
        run_id: discardRunId,
      })
    : { status: 0, json: null };
  const discarded = structured(discardedState.json);
  record(
    "isolatedDiscard",
    discardSubmit.status === 200 &&
      discardTerminal?.state === "completed" &&
      discardResponse.status === 200 &&
      discarded?.execution?.promotionState === "discarded" &&
      !fs.existsSync(discardFile),
    { runId: discardRunId, state: discardTerminal?.state, discardStatus: discardResponse.status }
  );

  // Idle steering is explicitly non-cancelling and becomes durable queue state.
  const steer = await call("ptah_steer", {
    request_id: `${prefix}-idle-steer`,
    session_id: hostSessionId,
    workspace,
    text: "record this as a follow-up without starting a second turn",
  });
  const steerState = structured(steer.json);
  record(
    "idleSteerQueues",
    steer.status === 200 && ["queued", "pending"].includes(steerState?.disposition),
    steerState
  );
  const queueArgs = {
    request_id: `${prefix}-queue`,
    session_id: hostSessionId,
    workspace,
    prompt: "record one queued follow-up",
  };
  const queueOne = await call("ptah_queue_prompt", queueArgs);
  const queueTwo = await call("ptah_queue_prompt", queueArgs);
  record(
    "queueIdempotency",
    queueOne.status === 200 && queueTwo.status === 200 &&
      JSON.stringify(structured(queueOne.json)) === JSON.stringify(structured(queueTwo.json)),
    { first: queueOne.status, replay: queueTwo.status }
  );

  // A new MCP transport session must still be able to read the durable run.
  const oldSession = mcpSession;
  const deleted = await fetch(endpoint, {
    method: "DELETE",
    headers: { Authorization: `Bearer ${token}`, "mcp-session-id": oldSession },
  });
  const stale = await mcpFetch("tools/list", {}, { sessionId: oldSession });
  record("sessionReconnectFailClosed", deleted.status === 204 && stale.status >= 400, {
    deleteStatus: deleted.status,
    staleStatus: stale.status,
  });
  const reconnect = await mcpFetch("initialize", {
    protocolVersion: "2025-11-25",
    capabilities: {},
    clientInfo: { name: "grokptah-reference-coordinator-reconnect", version: "1.0.0" },
  });
  mcpSession = reconnect.sessionId;
  const reconnectRead = isolatedRunId
    ? await call("ptah_get_run", { run_id: isolatedRunId })
    : { status: 0, json: null };
  record(
    "durableReadAfterReconnect",
    reconnect.status === 200 && !!mcpSession && reconnectRead.status === 200 &&
      structured(reconnectRead.json)?.runId === isolatedRunId,
    { sessionId: mcpSession, runId: isolatedRunId }
  );

  clearTimeout(watchdog);
  const failed = Object.entries(checks)
    .filter(([, ok]) => !ok)
    .map(([name]) => name);
  console.log(
    JSON.stringify({
      ok: failed.length === 0,
      failed,
      checks,
      steps,
      sharedRunId,
      isolatedRunId,
      toolCount: names.length,
      coordinator: "reference-fetch-mcp-streamable-http",
    })
  );
  process.exit(failed.length === 0 ? 0 : 1);
} catch (error) {
  clearTimeout(watchdog);
  console.error(error);
  console.log(JSON.stringify({ ok: false, error: String(error), checks, steps }));
  process.exit(1);
}
