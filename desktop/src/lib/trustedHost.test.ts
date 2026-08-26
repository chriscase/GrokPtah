import { describe, expect, it, vi } from "vitest";
import { CAPABILITY_CONTRACT, type CapabilityDescriptor } from "./capabilities";
import { GrokPtahClient } from "./grokptahClient";
import { GrokPtahCapabilityError } from "./grokptahOperations";
import {
  GROKPTAH_HOST_CONTRACT,
  GROKPTAH_RECOVERY_POLL_TOOL,
  GrokPtahHost,
  GrokPtahScopeError,
  applyGrokPtahRunNotification,
  assertGrokPtahRunScope,
  assertGrokPtahScope,
  createGrokPtahRunMonitor,
  negotiateGrokPtahCapabilities,
  parseGrokPtahRunScope,
  parseGrokPtahScope,
  requireGrokPtahCapabilities,
} from "./trustedHost";

const SCOPE = { sessionId: "session-1", workspace: "workspace-1" };
const RUN_SCOPE = { ...SCOPE, runId: "run-1" };

function capability(
  id: string,
  overrides: Partial<CapabilityDescriptor> = {},
): CapabilityDescriptor {
  return {
    id,
    tier: "review",
    mutating: false,
    human_gate: false,
    availability: "available",
    description: `synthetic ${id}`,
    ...overrides,
  };
}

const REVIEW = capability("run.review");
const QUEUE = capability("run.queue", { tier: "execute", mutating: true });
const AGENT_RESUME = capability("agent.resume", { tier: "execute", mutating: true });
const PROMOTE = capability("run.promote", {
  tier: "promote",
  mutating: true,
  human_gate: true,
  availability: "gated",
});

type ToolCall = { name: string; args: Record<string, unknown> };

/** A synthetic MCP endpoint: no sockets, no live service, no real credential. */
function transport(capabilities: CapabilityDescriptor[], streamBody?: string) {
  const tools: ToolCall[] = [];
  const closes: number[] = [];
  const fetcher = vi.fn<typeof fetch>(async (input, init) => {
    const method = init?.method ?? "GET";
    const headers = {
      "content-type": "application/json",
      "mcp-session-id": "transport-1",
    };
    if (method === "GET") {
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(new TextEncoder().encode(streamBody ?? ""));
          controller.close();
        },
      });
      return new Response(stream, {
        status: 200,
        headers: { "content-type": "text/event-stream" },
      });
    }
    if (method === "DELETE") {
      closes.push(closes.length + 1);
      return new Response(null, { status: 204 });
    }
    const request = JSON.parse(String(init?.body));
    if (request.method === "initialize") {
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
    tools.push({ name: request.params.name, args: request.params.arguments });
    return new Response(
      JSON.stringify({
        jsonrpc: "2.0",
        id: request.id,
        result: { structuredContent: { tool: request.params.name } },
      }),
      { status: 200, headers },
    );
  });
  return { tools, closes, fetcher };
}

function hostWith(capabilities: CapabilityDescriptor[], streamBody?: string) {
  const wire = transport(capabilities, streamBody);
  const host = new GrokPtahHost({
    client: new GrokPtahClient({
      baseUrl: "http://127.0.0.1:39200/mcp",
      token: "secret",
      fetcher: wire.fetcher,
    }),
  });
  return { ...wire, host };
}

function eventFrame(seq: number): string {
  return (
    `id: ${seq}\ndata: ` +
    JSON.stringify({
      jsonrpc: "2.0",
      method: "notifications/ptah_event",
      params: {
        sessionId: RUN_SCOPE.sessionId,
        workspace: RUN_SCOPE.workspace,
        runId: RUN_SCOPE.runId,
        seq,
        ts: "2026-08-24T00:00:00Z",
        update: { type: "progress" },
      },
    }) +
    "\n\n"
  );
}

function recoveryFrame(afterSeq: number, pollTool = GROKPTAH_RECOVERY_POLL_TOOL): string {
  return (
    "data: " +
    JSON.stringify({
      jsonrpc: "2.0",
      method: "notifications/ptah_recovery",
      params: {
        sessionId: RUN_SCOPE.sessionId,
        workspace: RUN_SCOPE.workspace,
        runId: RUN_SCOPE.runId,
        afterSeq,
        reason: "gap",
        pollTool,
      },
    }) +
    "\n\n"
  );
}

describe("trusted-host scope fence", () => {
  it("accepts an exact workspace and run fence", () => {
    expect(parseGrokPtahScope(SCOPE)).toEqual(SCOPE);
    expect(parseGrokPtahRunScope(RUN_SCOPE)).toEqual(RUN_SCOPE);
    expect(assertGrokPtahRunScope(RUN_SCOPE)).toEqual(RUN_SCOPE);
  });

  it.each([
    ["a non-object", null],
    ["a string", "session-1"],
    ["an array", []],
    ["an empty record", {}],
    ["a missing workspace", { sessionId: "session-1" }],
    ["an empty workspace", { sessionId: "session-1", workspace: "" }],
    ["a whitespace workspace", { sessionId: "session-1", workspace: "   " }],
    ["a non-string workspace", { sessionId: "session-1", workspace: 7 }],
    ["an over-specified fence", { ...SCOPE, runId: "run-1" }],
    ["a smuggled credential field", { ...SCOPE, token: "secret" }],
    ["a control character", { sessionId: "session-1", workspace: "work\nspace" }],
    ["an oversized field", { sessionId: "session-1", workspace: "w".repeat(513) }],
  ])("fails closed on %s", (_label, value) => {
    expect(parseGrokPtahScope(value)).toBeNull();
    expect(() => assertGrokPtahScope(value)).toThrow(GrokPtahScopeError);
  });

  it("refuses a run fence with no run id", () => {
    expect(parseGrokPtahRunScope(SCOPE)).toBeNull();
    expect(() => assertGrokPtahRunScope(SCOPE)).toThrow(GrokPtahScopeError);
  });

  it("names the offending field", () => {
    let error: unknown;
    try {
      assertGrokPtahScope({ ...SCOPE, token: "secret" });
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(GrokPtahScopeError);
    expect((error as GrokPtahScopeError).field).toBe("token");
  });
});

describe("trusted-host capability negotiation", () => {
  it("reports the lattice state per requirement", () => {
    const set = { contract: CAPABILITY_CONTRACT, capabilities: [REVIEW, PROMOTE] } as const;
    const report = negotiateGrokPtahCapabilities(set, [
      "run.review",
      "run.promote",
      "computer.control",
    ]);
    expect(report.contract).toBe(CAPABILITY_CONTRACT);
    expect(report.ready).toEqual(["run.review"]);
    expect(report.requiresGate).toEqual(["run.promote"]);
    expect(report.unavailable).toEqual(["computer.control"]);
    expect(report.outcomes[0].descriptor).toEqual(REVIEW);
    expect(report.outcomes[2].descriptor).toBeUndefined();
  });

  it("admits a gated capability only when its gate is held", () => {
    const set = { contract: CAPABILITY_CONTRACT, capabilities: [PROMOTE] } as const;
    expect(() => requireGrokPtahCapabilities(set, ["run.promote"])).toThrow(
      GrokPtahCapabilityError,
    );
    expect(
      requireGrokPtahCapabilities(set, [{ id: "run.promote", gateSatisfied: true }]).ready,
    ).toEqual(["run.promote"]);
  });

  it("treats an un-negotiated set as authorizing nothing", () => {
    expect(negotiateGrokPtahCapabilities(null, ["run.review"]).unavailable).toEqual([
      "run.review",
    ]);
    expect(negotiateGrokPtahCapabilities(undefined, ["run.review"]).unavailable).toEqual([
      "run.review",
    ]);
    let error: unknown;
    try {
      requireGrokPtahCapabilities(null, ["run.review"]);
    } catch (caught) {
      error = caught;
    }
    expect(error).toBeInstanceOf(GrokPtahCapabilityError);
    expect((error as GrokPtahCapabilityError).state).toBe("unavailable");
  });

  it("rejects a malformed requirement rather than defaulting it open", () => {
    expect(() =>
      negotiateGrokPtahCapabilities(null, [{ gateSatisfied: true } as never]),
    ).toThrow(TypeError);
  });
});

describe("GrokPtahHost", () => {
  it("negotiates on connect and exposes the contract", async () => {
    const { host } = hostWith([REVIEW, QUEUE]);
    const report = await host.connect(["run.review", "run.queue"]);
    expect(report.ready).toEqual(["run.review", "run.queue"]);
    expect(host.isConnected).toBe(true);
    expect(host.capabilities?.contract).toBe(CAPABILITY_CONTRACT);
    expect(GROKPTAH_HOST_CONTRACT).toBe("grokptah.host.v1");
  });

  it("closes the authenticated session when a required capability is missing", async () => {
    const { host, closes } = hostWith([REVIEW]);
    await expect(host.connect(["computer.control"])).rejects.toThrow(GrokPtahCapabilityError);
    expect(closes).toHaveLength(1);
    expect(host.isConnected).toBe(false);
  });

  it("binds queue and durable-agent calls to the validated fence", async () => {
    const { host, tools } = hostWith([QUEUE, AGENT_RESUME]);
    await host.connect();
    const workspace = host.workspace(SCOPE);

    await workspace.queuePrompt("request-1", "review", true);
    expect(tools.at(-1)).toEqual({
      name: "ptah_queue_prompt",
      args: {
        request_id: "request-1",
        session_id: SCOPE.sessionId,
        workspace: SCOPE.workspace,
        prompt: "review",
        priority: true,
      },
    });

    await workspace.resumePersistentAgent("agent-1", "request-2", "continue", 4);
    expect(tools.at(-1)).toEqual({
      name: "ptah_resume_persistent_agent",
      args: {
        request_id: "request-2",
        session_id: SCOPE.sessionId,
        workspace: SCOPE.workspace,
        agent_id: "agent-1",
        prompt: "continue",
        max_rounds: 4,
      },
    });
  });

  it("narrows a workspace fence to a run without re-supplying identity", async () => {
    const { host } = hostWith([REVIEW]);
    await host.connect();
    expect(host.workspace(SCOPE).run("run-1").scope).toEqual(RUN_SCOPE);
    expect(() => host.workspace(SCOPE).run("  ")).toThrow(GrokPtahScopeError);
  });

  it("keeps the approval gate on the run fence", async () => {
    const { host, tools } = hostWith([REVIEW, PROMOTE]);
    await host.connect();
    const run = host.run(RUN_SCOPE);

    await run.review();
    expect(tools.at(-1)?.name).toBe("ptah_review_run");

    await expect(run.approve("request-1", "source", "final", [])).rejects.toMatchObject({
      name: "GrokPtahCapabilityError",
      state: "requires_gate",
    });
    expect(tools.at(-1)?.name).toBe("ptah_review_run");

    await run.approve("request-1", "source", "final", [{ path: "a.ts", summary: "s" }], true);
    expect(tools.at(-1)).toEqual({
      name: "ptah_approve_run",
      args: {
        request_id: "request-1",
        session_id: RUN_SCOPE.sessionId,
        workspace: RUN_SCOPE.workspace,
        run_id: RUN_SCOPE.runId,
        source_fingerprint: "source",
        final_fingerprint: "final",
        changed_files: [{ path: "a.ts", summary: "s" }],
      },
    });
  });

  it("replays a bounded event page and refuses an unsupported poll tool", async () => {
    const { host, tools } = hostWith([REVIEW]);
    await host.connect();
    const run = host.run(RUN_SCOPE);

    await run.events({ afterSeq: 7, limit: 20 });
    expect(tools.at(-1)?.args).toEqual({
      session_id: RUN_SCOPE.sessionId,
      workspace: RUN_SCOPE.workspace,
      run_id: RUN_SCOPE.runId,
      after_seq: 7,
      limit: 20,
    });

    await run.replayRecovery({
      kind: "recovery",
      sseId: null,
      ...RUN_SCOPE,
      afterSeq: 7,
      reason: "gap",
      pollTool: GROKPTAH_RECOVERY_POLL_TOOL,
    });
    expect(tools.at(-1)?.args).toMatchObject({ after_seq: 7 });

    expect(() =>
      run.replayRecovery({
        kind: "recovery",
        sseId: null,
        ...RUN_SCOPE,
        afterSeq: 7,
        reason: "gap",
        pollTool: "ptah_promote_run",
      }),
    ).toThrow("unsupported poll tool");
  });

  it("refuses to stream without a negotiated review capability", async () => {
    const { host } = hostWith([QUEUE]);
    await host.connect();
    expect(() => host.run(RUN_SCOPE).stream()).toThrow(GrokPtahCapabilityError);
  });

  it("folds a contiguous stream and stops at recovery", async () => {
    const body = eventFrame(1) + eventFrame(2) + recoveryFrame(2);
    const { host } = hostWith([REVIEW], body);
    await host.connect();

    const updates = [];
    for await (const update of host.run(RUN_SCOPE).follow()) updates.push(update);

    expect(
      updates.map((update) => [
        update.notification.kind,
        update.state.lastSeq,
        update.state.recoveryRequired,
      ]),
    ).toEqual([
      ["event", 1, false],
      ["event", 2, false],
      ["recovery", 2, true],
    ]);
    expect(updates.at(-1)?.state.recovery?.pollTool).toBe(GROKPTAH_RECOVERY_POLL_TOOL);
    expect(updates[1].state.events).toHaveLength(2);
  });
});

describe("run monitor", () => {
  const event = (seq: number) =>
    ({
      kind: "event",
      sseId: seq,
      ...RUN_SCOPE,
      seq,
      ts: "2026-08-24T00:00:00Z",
      update: {},
    }) as const;

  it("advances only on a contiguous cursor", () => {
    const first = applyGrokPtahRunNotification(createGrokPtahRunMonitor(), event(1));
    expect(first?.lastSeq).toBe(1);
    expect(first?.recoveryRequired).toBe(false);
    expect(applyGrokPtahRunNotification(first!, event(2))?.lastSeq).toBe(2);
  });

  it("marks recovery on a gap instead of guessing the missing window", () => {
    const gapped = applyGrokPtahRunNotification(createGrokPtahRunMonitor(), event(4));
    expect(gapped?.recoveryRequired).toBe(true);
    expect(gapped?.lastSeq).toBe(0);
    expect(gapped?.events).toEqual([]);
  });

  it("refuses a stale frame outright", () => {
    expect(applyGrokPtahRunNotification(createGrokPtahRunMonitor(5), event(3))).toBeNull();
    expect(
      applyGrokPtahRunNotification(createGrokPtahRunMonitor(5), {
        kind: "recovery",
        sseId: null,
        ...RUN_SCOPE,
        afterSeq: 2,
        reason: "gap",
        pollTool: GROKPTAH_RECOVERY_POLL_TOOL,
      }),
    ).toBeNull();
  });

  it("seeds from the replay cursor the caller resumes at", () => {
    const monitor = createGrokPtahRunMonitor(9);
    expect(monitor.lastSeq).toBe(9);
    expect(applyGrokPtahRunNotification(monitor, event(10))?.lastSeq).toBe(10);
  });
});
