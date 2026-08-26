import { describe, expect, it, vi } from "vitest";
import { CAPABILITY_CONTRACT } from "./capabilities";
import { GrokPtahClient } from "./grokptahClient";
import {
  GrokPtahCapabilityError,
  GrokPtahOperations,
  validateGrokPtahBounds,
} from "./grokptahOperations";

function response(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

function clientWithCapabilities(fetcher: typeof fetch, capabilities: unknown[]) {
  const client = new GrokPtahClient({
    baseUrl: "http://127.0.0.1:39200/mcp",
    token: "secret",
    fetcher,
  });
  Object.defineProperty(client, "sessionId", { value: "transport-1", writable: true });
  Object.defineProperty(client, "protocolVersion", { value: "2025-03-26", writable: true });
  Object.defineProperty(client, "capabilitySet", {
    value: { contract: CAPABILITY_CONTRACT, capabilities },
    writable: true,
  });
  return client;
}

const execute = {
  id: "run.execute",
  tier: "execute",
  mutating: true,
  human_gate: false,
  availability: "available",
  description: "Submit runs",
};

const promote = {
  id: "run.promote",
  tier: "promote",
  mutating: true,
  human_gate: true,
  availability: "gated",
  description: "Promote isolated runs",
};

const computerControl = {
  id: "computer.control",
  tier: "computer_control",
  mutating: true,
  human_gate: true,
  availability: "gated",
  description: "Spend a host-issued human approval receipt to take control",
};

function lastRequestBody(fetcher: ReturnType<typeof vi.fn>): Record<string, unknown> {
  const call = fetcher.mock.calls.at(-1);
  const init = call?.[1] as RequestInit | undefined;
  return JSON.parse(String(init?.body ?? "{}")).params.arguments;
}

/** Fresh, id-correlated response per call: one Response can only be read once. */
function echoingFetcher(result: unknown) {
  return vi.fn<typeof fetch>().mockImplementation(async (_input, init) => {
    const { id } = JSON.parse(String((init as RequestInit | undefined)?.body ?? "{}"));
    return response({ jsonrpc: "2.0", id, result });
  });
}

describe("GrokPtahOperations", () => {
  it("rejects malformed execution bounds before transport", () => {
    expect(() => validateGrokPtahBounds({ maxRounds: 0 })).toThrow(
      "maxRounds must be a positive safe integer",
    );
    expect(() => validateGrokPtahBounds({ maxRounds: 25 })).toThrow(
      "maxRounds must be at most 24",
    );
    expect(() => validateGrokPtahBounds({ maxPromptBytes: 1.5 })).toThrow(
      "maxPromptBytes must be a positive safe integer",
    );
    expect(() => validateGrokPtahBounds({ maxDurationMs: -1 })).toThrow(
      "maxDurationMs must be a positive safe integer",
    );
  });

  it("builds a scope-fenced submit payload", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({
        jsonrpc: "2.0",
        id: 1,
        result: { runId: "run-1" },
      }),
    );
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [execute]));

    const result = await operations.submitTask(
      { sessionId: "session-1", workspace: "/repo" },
      "request-1",
      "review the change",
      { executionMode: "isolated_worktree", allowQueue: true },
    );

    expect(result.value).toEqual({ runId: "run-1" });
    const body = JSON.parse(String(fetcher.mock.calls[0][1]?.body));
    expect(body.params.arguments).toEqual({
      request_id: "request-1",
      session_id: "session-1",
      workspace: "/repo",
      prompt: "review the change",
      execution_mode: "isolated_worktree",
      allow_queue: true,
    });
  });

  it("validates bounds before sending a submit request", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [execute]));
    await expect(
      operations.submitTask(
        { sessionId: "session-1", workspace: "/repo" },
        "request-1",
        "review",
        { bounds: { maxRounds: 0 } },
      ),
    ).rejects.toThrow("maxRounds must be a positive safe integer");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("requires an explicit gate before promotion", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { promoted: true } }),
    );
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [promote]));
    const scope = { sessionId: "session-1", workspace: "/repo", runId: "run-1" };

    await expect(operations.promoteRun(scope, "request-1", "approval-1")).rejects.toEqual(
      expect.objectContaining<GrokPtahCapabilityError>({
        capabilityId: "run.promote",
        state: "requires_gate",
      }),
    );
    await expect(operations.promoteRun(scope, "request-1", "approval-1", true)).resolves.toEqual(
      expect.objectContaining({ value: { promoted: true } }),
    );
  });

  it("carries host-issued receipt material rather than a caller-asserted gate", async () => {
    const fetcher = echoingFetcher({ structuredContent: { state: "ready" } });
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [computerControl]),
    );
    const scope = { sessionId: "session-1", workspace: "/repo", runId: "run-1" };
    const lease = {
      actionClasses: ["semantic" as const],
      ttlMs: 30_000,
      usesRemaining: 2,
    };

    // Requesting the gate is reachable and grants nothing.
    await operations.requestComputerApproval(scope, "request-1", 3, lease);
    expect(lastRequestBody(fetcher)).toMatchObject({
      run_id: "run-1",
      expected_version: 3,
      uses_remaining: 2,
    });
    expect(lastRequestBody(fetcher)).not.toHaveProperty("approval_id");

    // Control carries the receipt the host issued.
    await operations.authorizeComputerRun(scope, "request-2", 3, lease, {
      approvalId: "approval-1",
      nonce: "n".repeat(64),
    });
    expect(lastRequestBody(fetcher)).toMatchObject({
      approval_id: "approval-1",
      approval_nonce: "n".repeat(64),
    });

    // There is no Boolean that stands in for the receipt.
    await expect(
      operations.authorizeComputerRun(scope, "request-3", 3, lease, {
        approvalId: "",
        nonce: "n".repeat(64),
      }),
    ).rejects.toThrow("receipt.approvalId must not be empty");
    await expect(
      operations.authorizeComputerRun(scope, "request-4", 3, lease, {
        approvalId: "approval-1",
        nonce: "  ",
      }),
    ).rejects.toThrow("receipt.nonce must not be empty");

    // A widening lease is still refused server-side, but obviously malformed
    // bounds never reach transport.
    await expect(
      operations.authorizeComputerRun(
        scope,
        "request-5",
        3,
        { ...lease, usesRemaining: 0 },
        { approvalId: "approval-1", nonce: "n".repeat(64) },
      ),
    ).rejects.toThrow("computer lease bounds must be positive safe integers");
  });

  it("keeps de-escalating computer controls reachable without a receipt", async () => {
    const fetcher = echoingFetcher({ structuredContent: { state: "paused" } });
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [computerControl]),
    );
    const scope = { sessionId: "session-1", workspace: "/repo", runId: "run-1" };

    await expect(operations.pauseComputerRun(scope, "request-1", 3)).resolves.toBeDefined();
    await expect(operations.takeOverComputerRun(scope, "request-2", 3)).resolves.toBeDefined();
    await expect(operations.cancelComputerRun(scope, "request-3", 3)).resolves.toBeDefined();
    expect(lastRequestBody(fetcher)).not.toHaveProperty("approval_id");
  });

  it("rejects empty identity fields before transport", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, [execute]));

    await expect(
      operations.getRun({ sessionId: "", workspace: "/repo", runId: "run-1" }),
    ).rejects.toThrow("sessionId must not be empty");
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("fails closed when the negotiated set omits a requested capability", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, []));
    await expect(
      operations.submitTask(
        { sessionId: "session-1", workspace: "/repo" },
        "request-1",
        "review",
      ),
    ).rejects.toMatchObject({ capabilityId: "run.execute", state: "unavailable" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("gates read operations on their negotiated capability", async () => {
    const fetcher = vi.fn<typeof fetch>();
    const operations = new GrokPtahOperations(clientWithCapabilities(fetcher, []));
    await expect(operations.listSessions()).rejects.toMatchObject({
      capabilityId: "session.observe",
    });
    await expect(
      operations.getRun({ sessionId: "session-1", workspace: "/repo", runId: "run-1" }),
    ).rejects.toMatchObject({ capabilityId: "run.review" });
    await expect(
      operations.getPersistentAgent({ sessionId: "session-1", workspace: "/repo" }, "agent-1"),
    ).rejects.toMatchObject({ capabilityId: "agent.continuity" });
    await expect(
      operations.listPersistentAgents({ sessionId: "session-1", workspace: "/repo" }),
    ).rejects.toMatchObject({ capabilityId: "agent.continuity" });
    expect(fetcher).not.toHaveBeenCalled();
  });

  it("uses the empty-args wire shape for allowlist-wide persistent-agent listing", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { agents: [] } }),
    );
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [
        {
          id: "agent.continuity",
          tier: "observe",
          mutating: false,
          human_gate: false,
          availability: "available",
          description: "List durable agents",
        },
      ]),
    );

    await operations.listPersistentAgents({ sessionId: "session-1", workspace: "/repo" });
    const body = JSON.parse(String(fetcher.mock.calls[0][1]?.body)) as {
      params?: { arguments?: unknown };
    };
    expect(body.params?.arguments).toEqual({});
  });

  it("exposes queue CAS operations through the same capability fence", async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(
      response({ jsonrpc: "2.0", id: 1, result: { ok: true } }),
    );
    const operations = new GrokPtahOperations(
      clientWithCapabilities(fetcher, [
        {
          id: "run.queue",
          tier: "execute",
          mutating: true,
          human_gate: false,
          availability: "available",
          description: "Queue",
        },
      ]),
    );
    await operations.reorderQueue(
      { sessionId: "session-1", workspace: "/repo" },
      "request-1",
      "entry-1",
      0,
      3,
      9,
    );
    expect(JSON.parse(String(fetcher.mock.calls[0][1]?.body)).params.arguments).toMatchObject({
      entry_id: "entry-1",
      to_index: 0,
      expected_version: 3,
      expected_revision: 9,
    });
  });
});
